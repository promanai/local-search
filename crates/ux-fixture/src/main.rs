#![forbid(unsafe_code)]

#[cfg(windows)]
mod windows {
    use std::{
        env,
        error::Error,
        fs,
        path::{Path, PathBuf},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use localsearch_catalog_index::{
        CATALOG_SCHEMA_ID, CatalogFingerprint, ProjectionRunSummary, ProjectionWorker,
        ProjectionWorkerError, ProjectionWorkerOptions,
    };
    use localsearch_core::{
        Availability, DocumentId, FileKey, FileLinkId, FilesystemEvent, VolumeId,
    };
    use localsearch_filesystem_graph::{
        FilesystemGraph, GraphMutation, GraphMutationBatch, VolumeState,
    };
    use localsearch_platform_core::{FilesystemProvider, VolumeDescriptor};
    use localsearch_windows_fs::WindowsFilesystemProvider;
    use serde::{Deserialize, Serialize};

    const STATE_SCHEMA_VERSION: u32 = 1;
    const FIXTURE_PREFIX: &str = "localsearch-ux-fixture-";
    const EXPECTED_LABEL: &str = "LS_TEST";
    const MAX_CHANGE_PASSES: usize = 128;
    const MIN_CHURN_SECONDS: u64 = 60;
    const MAX_CHURN_SECONDS: u64 = 1_800;

    const LONG_ENGLISH: &str = "this-is-an-intentionally-extremely-long-project-architecture-document-name-for-localsearch-ux-validation-2026-final-version.md";
    const LONG_RUSSIAN: &str = "Очень-длинное-название-документа-на-русском-для-проверки-интерфейса-и-ellipsis-LocalSearch-2026.md";
    const LONG_MIXED: &str =
        "PromanOS - Voice AI - Architecture - Production - Final - Reviewed - v27.md";

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct FixtureDocument {
        name: String,
        path: String,
        document_id: DocumentId,
        object_key: FileKey,
        file_link_id: FileLinkId,
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct FixtureState {
        schema_version: u32,
        run_id: String,
        volume_mount: PathBuf,
        fixture_root: PathBuf,
        graph_path: PathBuf,
        index_root: PathBuf,
        volume_id: VolumeId,
        long_names: Vec<FixtureDocument>,
        rename: FixtureDocument,
        rename_target: PathBuf,
        moved: FixtureDocument,
        move_target: PathBuf,
        deleted: FixtureDocument,
        last_scenario: Option<String>,
    }

    #[derive(Serialize)]
    struct OperationResult {
        operation: String,
        emitted_events: u64,
        projected_documents: u64,
        state_path: PathBuf,
        current_document: Option<FixtureDocument>,
        cleanup_complete: bool,
    }

    #[derive(Clone, Copy, Debug)]
    struct PumpOutcome {
        emitted_events: u64,
        backlog_before_projection: u64,
        applied_mutations: u64,
        projection_micros: u64,
        projection_run: bool,
    }

    #[derive(Serialize)]
    struct ChurnResult {
        operation: String,
        duration_millis: u64,
        minimum_cycle_interval_millis: u64,
        filesystem_operations: u64,
        provider_events: u64,
        projection_mutations: u64,
        projection_runs: u64,
        maximum_backlog_mutations: u64,
        maximum_projection_micros: u64,
        operations_per_second: f64,
        cleanup_required: bool,
    }

    #[derive(Serialize)]
    struct ProjectionSnapshot {
        operation: String,
        latest_sequence: u64,
        catalog_sequence: u64,
        content_sequence: Option<u64>,
        catalog_backlog: u64,
        content_backlog: Option<u64>,
        maximum_backlog: u64,
    }

    #[derive(Serialize)]
    struct ConvergenceResult {
        operation: String,
        desired_documents: u64,
        indexed_documents: u64,
        duplicate_documents: u64,
        payloads_match: bool,
        converged: bool,
    }

    #[derive(Default)]
    struct ChurnMetrics {
        provider_events: u64,
        projection_mutations: u64,
        projection_runs: u64,
        maximum_backlog_mutations: u64,
        maximum_projection_micros: u64,
    }

    impl ChurnMetrics {
        fn record(&mut self, outcome: PumpOutcome) {
            self.provider_events = self.provider_events.saturating_add(outcome.emitted_events);
            self.projection_mutations = self
                .projection_mutations
                .saturating_add(outcome.applied_mutations);
            self.projection_runs = self
                .projection_runs
                .saturating_add(u64::from(outcome.projection_run));
            self.maximum_backlog_mutations = self
                .maximum_backlog_mutations
                .max(outcome.backlog_before_projection);
            self.maximum_projection_micros = self
                .maximum_projection_micros
                .max(outcome.projection_micros);
        }
    }

    pub(super) fn main() -> Result<(), Box<dyn Error>> {
        let mut arguments = env::args().skip(1);
        let operation = arguments.next().ok_or(usage())?;
        let options = parse_options(arguments)?;
        match operation.as_str() {
            "init" => init(&options),
            "rename" | "move" | "delete" => mutate(&operation, &options),
            "offline" => offline(&options),
            "online" => online(&options),
            "churn" => churn(&options),
            "pump" => pump_command(&options),
            "snapshot" => snapshot(&options),
            "verify" => verify(&options),
            "cleanup" => cleanup(&options),
            _ => Err(usage().into()),
        }
    }

    fn usage() -> &'static str {
        "usage: localsearch-ux-fixture <init --volume L:\\ --run-root PATH | rename|move|delete|offline|online|pump|snapshot|verify|cleanup --state FILE | churn --state FILE --duration-seconds N --batch-files N [--cycle-interval-milliseconds N] [--projection-owner agent]>"
    }

    fn parse_options(
        mut arguments: impl Iterator<Item = String>,
    ) -> Result<Vec<(String, String)>, Box<dyn Error>> {
        let mut options = Vec::new();
        while let Some(key) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {key}"))?;
            options.push((key, value));
        }
        Ok(options)
    }

    fn required(options: &[(String, String)], name: &str) -> Result<String, Box<dyn Error>> {
        options
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| format!("{name} is required").into())
    }

    fn optional<'a>(options: &'a [(String, String)], name: &str) -> Option<&'a str> {
        options
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn init(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let volume_mount = validate_volume_root(Path::new(&required(options, "--volume")?))?;
        let run_root = absolute_path(Path::new(&required(options, "--run-root")?))?;
        if run_root.exists() {
            return Err(format!("run root already exists: {}", run_root.display()).into());
        }
        fs::create_dir_all(&run_root)?;
        let state_path = run_root.join("fixture-state.json");
        let graph_path = run_root.join("graph.sqlite3");
        let index_root = run_root.join("catalog");
        let run_id = format!(
            "{:x}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let fixture_root = volume_mount.join(format!("{FIXTURE_PREFIX}{run_id}"));
        if fixture_root.exists() {
            return Err(format!("fixture root already exists: {}", fixture_root.display()).into());
        }
        let initialized = (|| -> Result<(u64, u64), Box<dyn Error>> {
            create_files(&fixture_root)?;
            let provider = WindowsFilesystemProvider::new_with_usn_journal();
            let volume = discover_fixture_volume(&provider, &volume_mount)?;
            let mut events = Vec::new();
            let scan = provider.initial_scan(&volume, &mut |event| {
                events.push(event);
                Ok(())
            })?;
            let mut graph = FilesystemGraph::open(&graph_path)?;
            graph.ingest_snapshot(volume.clone(), scan.checkpoint, events)?;
            project(&graph, &index_root)?;

            let state = FixtureState {
                schema_version: STATE_SCHEMA_VERSION,
                run_id,
                volume_mount: volume_mount.clone(),
                fixture_root: fixture_root.clone(),
                graph_path,
                index_root,
                volume_id: volume.volume_id,
                long_names: [LONG_ENGLISH, LONG_RUSSIAN, LONG_MIXED]
                    .into_iter()
                    .map(|name| fixture_document(&graph, &fixture_root, name))
                    .collect::<Result<Vec<_>, _>>()?,
                rename: fixture_document(&graph, &fixture_root, "project-original.md")?,
                rename_target: fixture_root.join("normal/project-renamed.md"),
                moved: fixture_document(&graph, &fixture_root, "project-move.md")?,
                move_target: fixture_root.join("move-target/project-move.md"),
                deleted: fixture_document(&graph, &fixture_root, "project-delete.md")?,
                last_scenario: None,
            };
            let projected = u64::try_from(graph.desired_catalog_documents()?.len())?;
            write_state(&state_path, &state)?;
            Ok((scan.emitted_events, projected))
        })();
        let (emitted_events, projected_documents) = match initialized {
            Ok(result) => result,
            Err(error) => {
                validate_cleanup_target(&volume_mount, &fixture_root)?;
                if fixture_root.exists()
                    && let Err(cleanup) = fs::remove_dir_all(&fixture_root)
                {
                    return Err(format!(
                        "fixture initialization failed: {error}; rollback failed: {cleanup}"
                    )
                    .into());
                }
                return Err(error);
            }
        };
        print_json(&OperationResult {
            operation: "init".to_owned(),
            emitted_events,
            projected_documents,
            state_path,
            current_document: None,
            cleanup_complete: false,
        })
    }

    fn create_files(root: &Path) -> Result<(), Box<dyn Error>> {
        for directory in [
            "normal",
            "long-names",
            "move-source",
            "move-target",
            "delete",
            "deep-path/level-01/level-02/level-03/level-04",
        ] {
            fs::create_dir_all(root.join(directory))?;
        }
        for name in [LONG_ENGLISH, LONG_RUSSIAN, LONG_MIXED] {
            fs::write(
                root.join("long-names").join(name),
                b"LocalSearch UX fixture",
            )?;
        }
        fs::write(root.join("normal/project-original.md"), b"rename fixture")?;
        fs::write(root.join("move-source/project-move.md"), b"move fixture")?;
        fs::write(root.join("delete/project-delete.md"), b"delete fixture")?;
        fs::write(
            root.join("deep-path/level-01/level-02/level-03/level-04/deep-project.md"),
            b"deep path fixture",
        )?;
        Ok(())
    }

    fn mutate(operation: &str, options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let mut state = read_state(&state_path)?;
        validate_state(&state)?;
        match operation {
            "rename" => fs::rename(
                state.fixture_root.join("normal/project-original.md"),
                &state.rename_target,
            )?,
            "move" => fs::rename(
                state.fixture_root.join("move-source/project-move.md"),
                &state.move_target,
            )?,
            "delete" => fs::remove_file(state.fixture_root.join("delete/project-delete.md"))?,
            _ => return Err("unsupported mutation".into()),
        }
        let pump = pump_state(&state, 1, true)?;
        let graph = FilesystemGraph::open(&state.graph_path)?;
        let current_document = match operation {
            "rename" => Some(current_document(
                &graph,
                &state.rename,
                &state.rename_target,
            )?),
            "move" => Some(current_document(&graph, &state.moved, &state.move_target)?),
            "delete" => {
                if graph
                    .desired_catalog_document(state.deleted.document_id)?
                    .is_some()
                {
                    return Err("deleted document remains in desired catalog state".into());
                }
                None
            }
            _ => None,
        };
        state.last_scenario = Some(operation.to_owned());
        write_state(&state_path, &state)?;
        print_json(&OperationResult {
            operation: operation.to_owned(),
            emitted_events: pump.emitted_events,
            projected_documents: u64::try_from(graph.desired_catalog_documents()?.len())?,
            state_path,
            current_document,
            cleanup_complete: false,
        })
    }

    fn pump_command(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        let pump = pump_state(&state, 0, true)?;
        let graph = FilesystemGraph::open(&state.graph_path)?;
        print_json(&OperationResult {
            operation: "pump".to_owned(),
            emitted_events: pump.emitted_events,
            projected_documents: u64::try_from(graph.desired_catalog_documents()?.len())?,
            state_path,
            current_document: None,
            cleanup_complete: false,
        })
    }

    fn offline(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        if state.volume_mount.exists() {
            return Err(
                "offline transition requires the selected VHDX volume to be detached".into(),
            );
        }
        let mut graph = FilesystemGraph::open(&state.graph_path)?;
        let checkpoint = graph
            .checkpoint(state.volume_id)?
            .ok_or("fixture graph has no provider checkpoint")?;
        graph.apply_batch(&GraphMutationBatch {
            volume_id: state.volume_id,
            checkpoint,
            mutations: vec![GraphMutation::SetVolumeState {
                volume_id: state.volume_id,
                state: VolumeState::Offline,
            }],
        })?;
        project(&graph, &state.index_root)?;
        let current = graph
            .desired_catalog_document(state.long_names[0].document_id)?
            .ok_or("offline transition lost the fixture document")?;
        if current.metadata.availability != Availability::Offline {
            return Err("offline volume did not project offline catalog availability".into());
        }
        print_json(&OperationResult {
            operation: "offline".to_owned(),
            emitted_events: 0,
            projected_documents: u64::try_from(graph.desired_catalog_documents()?.len())?,
            state_path,
            current_document: None,
            cleanup_complete: false,
        })
    }

    fn online(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        let provider = WindowsFilesystemProvider::new_with_usn_journal();
        let volume = discover_fixture_volume(&provider, &state.volume_mount)?;
        if volume.volume_id != state.volume_id {
            return Err("reattached volume identity differs from the fixture volume".into());
        }
        let mut events = Vec::new();
        let scan = provider.reconcile(&volume, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        let mut graph = FilesystemGraph::open(&state.graph_path)?;
        graph.ingest_snapshot(volume, scan.checkpoint, events)?;
        project(&graph, &state.index_root)?;
        let current = graph
            .desired_catalog_document(state.long_names[0].document_id)?
            .ok_or("online reconciliation lost stable fixture identity")?;
        if current.metadata.availability != Availability::Online {
            return Err("reattached volume did not project online catalog availability".into());
        }
        print_json(&OperationResult {
            operation: "online".to_owned(),
            emitted_events: scan.emitted_events,
            projected_documents: u64::try_from(graph.desired_catalog_documents()?.len())?,
            state_path,
            current_document: None,
            cleanup_complete: false,
        })
    }

    fn pump_state(
        state: &FixtureState,
        minimum_events: u64,
        project_locally: bool,
    ) -> Result<PumpOutcome, Box<dyn Error>> {
        let provider = WindowsFilesystemProvider::new_with_usn_journal();
        let _volume = discover_volume_id(&provider, state.volume_id)?;
        let mut graph = FilesystemGraph::open(&state.graph_path)?;
        let mut checkpoint = graph
            .checkpoint(state.volume_id)?
            .ok_or("fixture graph has no provider checkpoint")?;
        let started = Instant::now();
        let mut emitted = 0_u64;
        for _ in 0..MAX_CHANGE_PASSES {
            let mut events: Vec<FilesystemEvent> = Vec::new();
            let batch = provider.read_changes(&checkpoint, 4_096, &mut |event| {
                events.push(event);
                Ok(())
            })?;
            emitted = emitted.saturating_add(u64::from(batch.emitted_events));
            checkpoint = batch.checkpoint.clone();
            graph.apply_batch(&GraphMutationBatch::from_events(
                state.volume_id,
                batch.checkpoint,
                events,
            ))?;
            if batch.has_more {
                continue;
            }
            if emitted >= minimum_events || started.elapsed() >= Duration::from_secs(5) {
                let backlog_before_projection = projection_backlog(&graph)?;
                if !project_locally {
                    return Ok(PumpOutcome {
                        emitted_events: emitted,
                        backlog_before_projection,
                        applied_mutations: 0,
                        projection_micros: 0,
                        projection_run: false,
                    });
                }
                let projection_started = Instant::now();
                let projection = run_projection_with_retry(&state.index_root, &graph)?;
                if projection.backlog_remaining {
                    return Err("fixture projection left a durable backlog".into());
                }
                return Ok(PumpOutcome {
                    emitted_events: emitted,
                    backlog_before_projection,
                    applied_mutations: projection.applied_mutations,
                    projection_micros: u64::try_from(projection_started.elapsed().as_micros())
                        .unwrap_or(u64::MAX),
                    projection_run: true,
                });
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err("USN journal did not drain within the bounded pass count".into())
    }

    fn cleanup(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        validate_cleanup_target(&state.volume_mount, &state.fixture_root)?;
        if state.fixture_root.exists() {
            fs::remove_dir_all(&state.fixture_root)?;
        }
        let pump = pump_state(&state, 1, true)?;
        let graph = FilesystemGraph::open(&state.graph_path)?;
        let remaining = graph
            .desired_catalog_documents()?
            .into_iter()
            .filter(|document| path_is_within(&state.fixture_root, &document.resolved_path))
            .count();
        if remaining != 0 || state.fixture_root.exists() {
            return Err("fixture cleanup did not converge".into());
        }
        print_json(&OperationResult {
            operation: "cleanup".to_owned(),
            emitted_events: pump.emitted_events,
            projected_documents: u64::try_from(graph.desired_catalog_documents()?.len())?,
            state_path,
            current_document: None,
            cleanup_complete: true,
        })
    }

    fn churn(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        let duration_seconds = bounded_number(
            &required(options, "--duration-seconds")?,
            MIN_CHURN_SECONDS,
            MAX_CHURN_SECONDS,
            "duration-seconds",
        )?;
        let batch_files =
            bounded_number(&required(options, "--batch-files")?, 1, 256, "batch-files")?;
        let cycle_interval_millis = match optional(options, "--cycle-interval-milliseconds") {
            Some(value) => bounded_number(value, 50, 5_000, "cycle-interval-milliseconds")?,
            None => 250,
        };
        let cycle_interval = Duration::from_millis(cycle_interval_millis);
        let agent_owns_projection = match optional(options, "--projection-owner") {
            Some("agent") => true,
            Some(value) => return Err(format!("unsupported projection owner: {value}").into()),
            None => false,
        };
        let source = state.fixture_root.join("churn-source");
        let target = state.fixture_root.join("churn-target");
        fs::create_dir(&source)?;
        fs::create_dir(&target)?;

        let started = Instant::now();
        let deadline = Duration::from_secs(duration_seconds);
        let mut sequence = 0_u64;
        let mut filesystem_operations = 2_u64;
        let mut metrics = ChurnMetrics::default();
        metrics.record(pump_state(&state, 1, !agent_owns_projection)?);

        while started.elapsed() < deadline {
            let cycle_started = Instant::now();
            let mut names = Vec::with_capacity(usize::try_from(batch_files)?);
            for _ in 0..batch_files {
                sequence = sequence.saturating_add(1);
                let name = format!("architecture-churn-{sequence:012}.md");
                fs::write(source.join(&name), b"LocalSearch sustained UX load")?;
                names.push(name);
            }
            filesystem_operations = filesystem_operations.saturating_add(batch_files);
            metrics.record(pump_state(&state, 1, !agent_owns_projection)?);

            for name in &names {
                fs::rename(source.join(name), source.join(format!("renamed-{name}")))?;
            }
            filesystem_operations = filesystem_operations.saturating_add(batch_files);
            metrics.record(pump_state(&state, 1, !agent_owns_projection)?);

            for name in &names {
                let renamed = format!("renamed-{name}");
                fs::rename(source.join(&renamed), target.join(&renamed))?;
            }
            filesystem_operations = filesystem_operations.saturating_add(batch_files);
            metrics.record(pump_state(&state, 1, !agent_owns_projection)?);

            for name in &names {
                fs::remove_file(target.join(format!("renamed-{name}")))?;
            }
            filesystem_operations = filesystem_operations.saturating_add(batch_files);
            metrics.record(pump_state(&state, 1, !agent_owns_projection)?);
            if let Some(remaining) = cycle_interval.checked_sub(cycle_started.elapsed()) {
                thread::sleep(remaining);
            }
        }
        let elapsed = started.elapsed();
        print_json(&ChurnResult {
            operation: "churn".to_owned(),
            duration_millis: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            minimum_cycle_interval_millis: cycle_interval_millis,
            filesystem_operations,
            provider_events: metrics.provider_events,
            projection_mutations: metrics.projection_mutations,
            projection_runs: metrics.projection_runs,
            maximum_backlog_mutations: metrics.maximum_backlog_mutations,
            maximum_projection_micros: metrics.maximum_projection_micros,
            operations_per_second: f64::from(u32::try_from(filesystem_operations)?)
                / elapsed.as_secs_f64(),
            cleanup_required: true,
        })
    }

    fn projection_backlog(graph: &FilesystemGraph) -> Result<u64, Box<dyn Error>> {
        let latest = graph.latest_outbox_sequence()?.0;
        let applied = graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)?
            .map_or(0, |checkpoint| checkpoint.last_sequence);
        Ok(latest.saturating_sub(applied))
    }

    fn snapshot(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        const CONTENT_SCHEMA_ID: &str = "CONTENT-SCHEMA-v1";
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        let graph = FilesystemGraph::open_read_only(&state.graph_path)?;
        let latest = graph.latest_outbox_sequence()?.0;
        let catalog = graph
            .projector_checkpoint(CATALOG_SCHEMA_ID)?
            .map_or(0, |checkpoint| checkpoint.last_sequence);
        let content = graph
            .projector_checkpoint(CONTENT_SCHEMA_ID)?
            .map(|checkpoint| checkpoint.last_sequence);
        let catalog_backlog = latest.saturating_sub(catalog);
        let content_backlog = content.map(|sequence| latest.saturating_sub(sequence));
        let maximum_backlog =
            content_backlog.map_or(catalog_backlog, |value| catalog_backlog.max(value));
        print_json(&ProjectionSnapshot {
            operation: "snapshot".to_owned(),
            latest_sequence: latest,
            catalog_sequence: catalog,
            content_sequence: content,
            catalog_backlog,
            content_backlog,
            maximum_backlog,
        })
    }

    fn verify(options: &[(String, String)]) -> Result<(), Box<dyn Error>> {
        let state_path = absolute_path(Path::new(&required(options, "--state")?))?;
        let state = read_state(&state_path)?;
        validate_state(&state)?;
        let graph = FilesystemGraph::open_read_only(&state.graph_path)?;
        let desired = graph.desired_catalog_documents()?;
        let mut desired_fingerprint = CatalogFingerprint::default();
        for document in &desired {
            desired_fingerprint.add_desired(document)?;
        }
        let worker = ProjectionWorker::new(&state.index_root, ProjectionWorkerOptions::default());
        let indexed_fingerprint = worker.active_index(&graph)?.reader()?.fingerprint()?;
        let payloads_match = desired_fingerprint == indexed_fingerprint;
        let duplicates = indexed_fingerprint.duplicate_documents();
        let converged = payloads_match && duplicates == 0;
        print_json(&ConvergenceResult {
            operation: "verify".to_owned(),
            desired_documents: desired_fingerprint.documents,
            indexed_documents: indexed_fingerprint.documents,
            duplicate_documents: duplicates,
            payloads_match,
            converged,
        })
    }

    fn bounded_number(
        value: &str,
        minimum: u64,
        maximum: u64,
        name: &str,
    ) -> Result<u64, Box<dyn Error>> {
        let parsed = value.parse::<u64>()?;
        if !(minimum..=maximum).contains(&parsed) {
            return Err(format!("{name} must be between {minimum} and {maximum}").into());
        }
        Ok(parsed)
    }

    fn current_document(
        graph: &FilesystemGraph,
        stale: &FixtureDocument,
        expected_path: &Path,
    ) -> Result<FixtureDocument, Box<dyn Error>> {
        let document = graph
            .desired_catalog_document(stale.document_id)?
            .ok_or("stable document identity disappeared after rename or move")?;
        if document.identity.object_key != stale.object_key
            || document.identity.file_link_id != stale.file_link_id
            || comparable_text(&document.resolved_path) != comparable_path(expected_path)
        {
            return Err("rename or move did not preserve identity and current path".into());
        }
        Ok(FixtureDocument {
            name: document.name,
            path: document.resolved_path,
            document_id: document.identity.document_id,
            object_key: document.identity.object_key,
            file_link_id: document.identity.file_link_id,
        })
    }

    fn fixture_document(
        graph: &FilesystemGraph,
        fixture_root: &Path,
        name: &str,
    ) -> Result<FixtureDocument, Box<dyn Error>> {
        let matches = graph
            .desired_catalog_documents()?
            .into_iter()
            .filter(|document| {
                document.name == name && path_is_within(fixture_root, &document.resolved_path)
            })
            .collect::<Vec<_>>();
        let [document] = matches.as_slice() else {
            return Err(format!("expected exactly one fixture document named {name}").into());
        };
        Ok(FixtureDocument {
            name: document.name.clone(),
            path: document.resolved_path.clone(),
            document_id: document.identity.document_id,
            object_key: document.identity.object_key,
            file_link_id: document.identity.file_link_id,
        })
    }

    fn project(graph: &FilesystemGraph, index_root: &Path) -> Result<(), Box<dyn Error>> {
        let summary = run_projection_with_retry(index_root, graph)?;
        if summary.backlog_remaining {
            return Err("fixture projection left a durable backlog".into());
        }
        Ok(())
    }

    fn run_projection_with_retry(
        index_root: &Path,
        graph: &FilesystemGraph,
    ) -> Result<ProjectionRunSummary, Box<dyn Error>> {
        const MAX_ATTEMPTS: usize = 50;
        for attempt in 0..MAX_ATTEMPTS {
            let worker = ProjectionWorker::new(index_root, ProjectionWorkerOptions::default());
            match worker.run(graph) {
                Ok(summary) => return Ok(summary),
                Err(ProjectionWorkerError::Catalog(_)) if attempt + 1 < MAX_ATTEMPTS => {
                    // Windows security scanners and memory-mapped readers can briefly retain a
                    // newly-created Tantivy segment. A fresh writer attempt receives a new
                    // segment identity and preserves the idempotent outbox contract.
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err("fixture projection retry budget exhausted".into())
    }

    fn discover_fixture_volume(
        provider: &WindowsFilesystemProvider,
        mount: &Path,
    ) -> Result<VolumeDescriptor, Box<dyn Error>> {
        let expected = mount.to_string_lossy();
        let volume = provider
            .discover_volumes()?
            .into_iter()
            .find(|candidate| {
                candidate
                    .mount_points
                    .iter()
                    .any(|point| point.eq_ignore_ascii_case(&expected))
            })
            .ok_or("fixture volume was not discovered")?;
        if volume.display_name.as_deref() != Some(EXPECTED_LABEL)
            || !volume
                .filesystem
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("NTFS"))
            || !volume.local
        {
            return Err("fixture refuses a volume that is not local NTFS labelled LS_TEST".into());
        }
        Ok(volume)
    }

    fn discover_volume_id(
        provider: &WindowsFilesystemProvider,
        volume_id: VolumeId,
    ) -> Result<VolumeDescriptor, Box<dyn Error>> {
        provider
            .discover_volumes()?
            .into_iter()
            .find(|volume| volume.volume_id == volume_id)
            .ok_or_else(|| "fixture volume is offline".into())
    }

    fn validate_volume_root(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
        let display = path.to_string_lossy();
        let bytes = display.as_bytes();
        if bytes.len() != 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
            || bytes[0].eq_ignore_ascii_case(&b'c')
        {
            return Err("fixture requires a non-C drive root such as L:\\".into());
        }
        Ok(PathBuf::from(format!("{}:\\", char::from(bytes[0]))))
    }

    fn validate_state(state: &FixtureState) -> Result<(), Box<dyn Error>> {
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err("unsupported fixture state version".into());
        }
        validate_cleanup_target(&state.volume_mount, &state.fixture_root)
    }

    fn validate_cleanup_target(volume: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
        if target.parent() != Some(volume)
            || !target
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(FIXTURE_PREFIX))
        {
            return Err("fixture target is outside the explicitly selected volume root".into());
        }
        Ok(())
    }

    fn path_is_within(root: &Path, candidate: &str) -> bool {
        let root = comparable_path(root);
        let candidate = comparable_text(candidate);
        candidate == root
            || candidate.strip_prefix(&root).is_some_and(|suffix| {
                suffix.starts_with(std::path::MAIN_SEPARATOR) || suffix.starts_with('/')
            })
    }

    fn absolute_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
        if path.is_absolute() {
            Ok(path.to_owned())
        } else {
            Ok(env::current_dir()?.join(path))
        }
    }

    fn comparable_path(path: &Path) -> String {
        comparable_text(&path.to_string_lossy())
    }

    fn comparable_text(value: &str) -> String {
        value.replace('\\', "/").trim_end_matches('/').to_owned()
    }

    fn read_state(path: &Path) -> Result<FixtureState, Box<dyn Error>> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn write_state(path: &Path, state: &FixtureState) -> Result<(), Box<dyn Error>> {
        fs::write(path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    fn print_json(value: &impl Serialize) -> Result<(), Box<dyn Error>> {
        println!("{}", serde_json::to_string_pretty(value)?);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use std::path::Path;

        use super::{ChurnMetrics, PumpOutcome, bounded_number, path_is_within};

        #[test]
        fn churn_metrics_are_monotonic_and_keep_the_worst_observation() {
            let mut metrics = ChurnMetrics::default();
            metrics.record(PumpOutcome {
                emitted_events: 7,
                backlog_before_projection: 11,
                applied_mutations: 9,
                projection_micros: 400,
                projection_run: true,
            });
            metrics.record(PumpOutcome {
                emitted_events: 5,
                backlog_before_projection: 3,
                applied_mutations: 4,
                projection_micros: 900,
                projection_run: true,
            });
            assert_eq!(metrics.provider_events, 12);
            assert_eq!(metrics.projection_mutations, 13);
            assert_eq!(metrics.projection_runs, 2);
            assert_eq!(metrics.maximum_backlog_mutations, 11);
            assert_eq!(metrics.maximum_projection_micros, 900);
        }

        #[test]
        fn churn_bounds_reject_unbounded_operator_input() {
            assert_eq!(
                bounded_number("60", 60, 1_800, "duration").expect("minimum is valid"),
                60
            );
            assert!(bounded_number("59", 60, 1_800, "duration").is_err());
            assert!(bounded_number("1801", 60, 1_800, "duration").is_err());
            assert_eq!(
                bounded_number("250", 50, 5_000, "cycle interval")
                    .expect("default cycle interval is valid"),
                250
            );
            assert!(bounded_number("49", 50, 5_000, "cycle interval").is_err());
            assert!(bounded_number("5001", 50, 5_000, "cycle interval").is_err());
        }

        #[test]
        fn fixture_scope_does_not_match_sibling_prefix_or_prior_run() {
            let root = Path::new(r"L:\localsearch-ux-fixture-current");
            assert!(path_is_within(
                root,
                r"L:\localsearch-ux-fixture-current\long-names\architecture.md"
            ));
            assert!(!path_is_within(
                root,
                r"L:\localsearch-ux-fixture-current-old\long-names\architecture.md"
            ));
            assert!(!path_is_within(
                root,
                r"L:\localsearch-ux-fixture-prior\long-names\architecture.md"
            ));
        }
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    windows::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LocalSearch real-filesystem UX fixture requires Windows");
    std::process::exit(2);
}
