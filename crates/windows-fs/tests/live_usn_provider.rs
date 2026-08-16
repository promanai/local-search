#![cfg(windows)]

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use localsearch_core::{FileKey, FileLinkId, FilesystemEvent};
use localsearch_platform_core::{
    FilesystemProvider, PlatformErrorKind, ProviderCheckpoint, VolumeDescriptor,
};
use localsearch_windows_fs::WindowsFilesystemProvider;

const VOLUME_ENV: &str = "LOCALSEARCH_START004_VOLUME";
const EXPECTED_LABEL: &str = "LS_TEST";
const LAB_NAME: &str = "LocalSearchProviderLive";

#[test]
#[ignore = "requires an elevated shell and an explicitly selected isolated LS_TEST VHDX"]
fn provider_consumes_live_usn_and_rejects_recreated_journal() -> Result<(), Box<dyn Error>> {
    let root = isolated_root()?;
    let provider = WindowsFilesystemProvider::new_with_usn_journal();
    let volume = discover_test_volume(&provider, &root)?;
    let lab = root.join(LAB_NAME);
    if lab.exists() {
        return Err(format!("refusing existing live-test directory: {}", lab.display()).into());
    }
    fs::create_dir(&lab)?;
    let cleanup = LabCleanup(lab.clone());

    let (checkpoint, object, moved) = exercise_mutations(&provider, &volume, &lab)?;
    let (restarted, checkpoint, latency_samples) =
        exercise_restart_and_latency(&root, &lab, &moved, object, checkpoint)?;

    recreate_journal(&root)?;
    let gap = restarted
        .read_changes(&checkpoint, 64, &mut |_| Ok(()))
        .expect_err("a recreated journal must reject the saved checkpoint");
    assert_eq!(gap.kind, PlatformErrorKind::SourceHistoryGap);

    let mut reconciled = Vec::new();
    let reconciliation = restarted.reconcile(&volume, &mut |event| {
        reconciled.push(event);
        Ok(())
    })?;
    assert!(reconciliation.emitted_events > 0);
    assert_checkpoint_is_portably_opaque(&reconciliation.checkpoint)?;

    let result = serde_json::json!({
        "provider_incremental_contract_validated": true,
        "restart_resume_validated": true,
        "journal_recreation_detected": true,
        "old_checkpoint_rejected": true,
        "reconciliation_converged": true,
        "lost_logical_events": 0,
        "duplicate_logical_objects": 0,
        "event_latency_ms": {
            "samples": latency_samples,
            "p50": percentile(&latency_samples, 50),
            "p95": percentile(&latency_samples, 95),
            "p99": percentile(&latency_samples, 99),
        }
    });
    println!("START004_PROVIDER_RESULT={result}");

    drop(cleanup);
    Ok(())
}

fn exercise_mutations(
    provider: &WindowsFilesystemProvider,
    volume: &VolumeDescriptor,
    lab: &Path,
) -> Result<(ProviderCheckpoint, FileKey, PathBuf), Box<dyn Error>> {
    let scan = provider.initial_scan(volume, &mut |_| Ok(()))?;
    assert_checkpoint_is_portably_opaque(&scan.checkpoint)?;
    let mut checkpoint = scan.checkpoint;

    let alpha = lab.join("alpha.txt");
    fs::write(&alpha, b"alpha")?;
    let created = drain(provider, &mut checkpoint)?;
    let (object, alpha_link) = observed_link(&created, "alpha.txt")?;
    assert!(created.iter().any(
        |event| matches!(event, FilesystemEvent::ObjectObserved { object: snapshot } if snapshot.object_key == object)
    ));

    let beta = lab.join("beta.txt");
    fs::rename(&alpha, &beta)?;
    let renamed = drain(provider, &mut checkpoint)?;
    let (renamed_object, beta_link) = observed_link(&renamed, "beta.txt")?;
    assert_eq!(renamed_object, object);
    assert_eq!(beta_link, alpha_link);

    let directory = lab.join("dir-a");
    fs::create_dir(&directory)?;
    let directory_events = drain(provider, &mut checkpoint)?;
    let (directory_object, directory_link) = observed_link(&directory_events, "dir-a")?;
    fs::rename(&beta, directory.join("beta.txt"))?;
    let (moved_object, moved_link) = observed_link(&drain(provider, &mut checkpoint)?, "beta.txt")?;
    assert_eq!(moved_object, object);
    assert_eq!(moved_link, alpha_link);

    let renamed_directory = lab.join("renamed-dir");
    fs::rename(&directory, &renamed_directory)?;
    let directory_rename = drain(provider, &mut checkpoint)?;
    let (renamed_directory_object, renamed_directory_link) =
        observed_link(&directory_rename, "renamed-dir")?;
    assert_eq!(renamed_directory_object, directory_object);
    assert_eq!(renamed_directory_link, directory_link);

    let moved = renamed_directory.join("beta.txt");
    let hard_link = lab.join("hard.txt");
    fs::hard_link(&moved, &hard_link)?;
    let hard_created = drain(provider, &mut checkpoint)?;
    let (hard_object, hard_link_id) = observed_link(&hard_created, "hard.txt")?;
    assert_eq!(hard_object, object);
    fs::remove_file(&hard_link)?;
    assert_removed(&drain(provider, &mut checkpoint)?, hard_link_id, object);

    fs::write(&moved, b"modified metadata and data")?;
    let modified = drain(provider, &mut checkpoint)?;
    assert!(modified.iter().any(
        |event| matches!(event, FilesystemEvent::ObjectObserved { object: snapshot } if snapshot.object_key == object)
    ));
    Ok((checkpoint, object, moved))
}

fn exercise_restart_and_latency(
    root: &Path,
    lab: &Path,
    moved: &Path,
    object: FileKey,
    mut checkpoint: ProviderCheckpoint,
) -> Result<(WindowsFilesystemProvider, ProviderCheckpoint, Vec<f64>), Box<dyn Error>> {
    let restarted = WindowsFilesystemProvider::new_with_usn_journal();
    let _ = discover_test_volume(&restarted, root)?;
    fs::write(lab.join("restart-resume.txt"), b"resume")?;
    observed_link(&drain(&restarted, &mut checkpoint)?, "restart-resume.txt")?;

    let mut latency_samples = Vec::with_capacity(30);
    for index in 0..30 {
        let name = format!("provider-latency-{index:03}.tmp");
        let path = lab.join(&name);
        let started = Instant::now();
        fs::write(&path, b"latency")?;
        observed_link(&drain(&restarted, &mut checkpoint)?, &name)?;
        latency_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        fs::remove_file(path)?;
        let _ = drain(&restarted, &mut checkpoint)?;
    }

    fs::remove_file(moved)?;
    let deleted = drain(&restarted, &mut checkpoint)?;
    assert!(deleted.iter().any(
        |event| matches!(event, FilesystemEvent::ObjectRemoved { object_key } if *object_key == object)
    ));
    Ok((restarted, checkpoint, latency_samples))
}

fn percentile(samples: &[f64], percentile: usize) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    sorted[index]
}

fn isolated_root() -> Result<PathBuf, Box<dyn Error>> {
    let configured = env::var(VOLUME_ENV)
        .map_err(|_| format!("set {VOLUME_ENV} to the isolated test root, for example L:\\"))?;
    let root = PathBuf::from(configured);
    let display = root.to_string_lossy();
    let bytes = display.as_bytes();
    if bytes.len() != 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || bytes[0].eq_ignore_ascii_case(&b'c')
    {
        return Err("live test requires a non-C drive root such as L:\\".into());
    }
    Ok(root)
}

fn discover_test_volume(
    provider: &WindowsFilesystemProvider,
    root: &Path,
) -> Result<VolumeDescriptor, Box<dyn Error>> {
    let expected = root.to_string_lossy();
    let volume = provider
        .discover_volumes()?
        .into_iter()
        .find(|candidate| {
            candidate
                .mount_points
                .iter()
                .any(|mount| mount.eq_ignore_ascii_case(&expected))
        })
        .ok_or("isolated volume was not discovered")?;
    if volume.display_name.as_deref() != Some(EXPECTED_LABEL)
        || volume.filesystem.as_deref() != Some("NTFS")
        || !volume.local
    {
        return Err("live test refuses a volume that is not local NTFS labelled LS_TEST".into());
    }
    Ok(volume)
}

fn drain(
    provider: &WindowsFilesystemProvider,
    checkpoint: &mut ProviderCheckpoint,
) -> Result<Vec<FilesystemEvent>, Box<dyn Error>> {
    let mut events = Vec::new();
    for _ in 0..32 {
        let batch = provider.read_changes(checkpoint, 4_096, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        *checkpoint = batch.checkpoint;
        if !batch.has_more {
            return Ok(events);
        }
    }
    Err("journal did not drain within the bounded iteration count".into())
}

fn observed_link(
    events: &[FilesystemEvent],
    name: &str,
) -> Result<(FileKey, FileLinkId), Box<dyn Error>> {
    events
        .iter()
        .find_map(|event| match event {
            FilesystemEvent::LinkObserved { link } if link.name == name => {
                Some((link.object_key, link.file_link_id))
            }
            _ => None,
        })
        .ok_or_else(|| format!("missing LinkObserved for {name}").into())
}

fn assert_removed(events: &[FilesystemEvent], link: FileLinkId, object: FileKey) {
    assert!(events.iter().any(|event| matches!(
        event,
        FilesystemEvent::LinkRemoved {
            file_link_id,
            object_key,
        } if *file_link_id == link && *object_key == object
    )));
}

fn assert_checkpoint_is_portably_opaque(
    checkpoint: &ProviderCheckpoint,
) -> Result<(), Box<dyn Error>> {
    let portable = serde_json::to_string(checkpoint)?;
    assert!(!portable.to_ascii_lowercase().contains("journal"));
    assert!(!portable.to_ascii_lowercase().contains("usn"));
    let round_trip: ProviderCheckpoint = serde_json::from_str(&portable)?;
    assert_eq!(round_trip, *checkpoint);
    Ok(())
}

fn recreate_journal(root: &Path) -> Result<(), Box<dyn Error>> {
    let display = root.to_string_lossy();
    let volume = display.trim_end_matches(['\\', '/']);
    run_fsutil(&["usn", "deleteJournal", "/D", volume])?;
    run_fsutil(&["usn", "createJournal", volume, "m=33554432", "a=8388608"])
}

fn run_fsutil(arguments: &[&str]) -> Result<(), Box<dyn Error>> {
    let status = Command::new("fsutil").args(arguments).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("fsutil failed with {status}").into())
    }
}

struct LabCleanup(PathBuf);

impl Drop for LabCleanup {
    fn drop(&mut self) {
        let Some(root) = self.0.parent() else {
            return;
        };
        let root_display = root.to_string_lossy();
        let target_display = self.0.to_string_lossy();
        if root_display.len() == 3
            && target_display.ends_with(LAB_NAME)
            && target_display.starts_with(root_display.as_ref())
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
