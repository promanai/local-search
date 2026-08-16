use localsearch_core::{FileKey, FilesystemEvent};
use serde::{Deserialize, Serialize};

use crate::{FilesystemProvider, PlatformErrorKind, PlatformResult, ProviderCheckpoint};

/// Mutations every stateful provider fixture must be able to exercise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractMutation {
    Create,
    Metadata,
    Rename,
    Move,
    DirectoryRename,
    Delete,
    HardLinkCreate,
    HardLinkDelete,
}

/// Machine-readable result of the shared provider behavioral contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderContractReport {
    pub assertions: u32,
    pub scenarios: Vec<String>,
}

/// Test control surface implemented by deterministic or isolated live-volume fixtures.
pub trait ProviderContractFixture: FilesystemProvider {
    /// Applies one externally observable source mutation.
    ///
    /// # Errors
    /// Returns a provider error when the isolated source cannot apply the mutation.
    fn apply(&mut self, mutation: ContractMutation) -> PlatformResult<()>;
    /// Creates a new provider process view over the same durable source state.
    #[must_use]
    fn restart(&self) -> Self
    where
        Self: Sized;
    fn recreate_history(&mut self);
    fn set_online(&mut self, online: bool);
    fn primary_object(&self) -> FileKey;
    fn projected_state(&self) -> Vec<FilesystemEvent>;
}

/// Runs the lifecycle requirements shared by Windows and future providers.
///
/// # Errors
/// Returns the first provider or serialization error exposed by the fixture.
///
/// # Panics
/// Panics when a provider violates an asserted behavioral contract. This is intentionally a test
/// harness: a contract violation must fail the invoking test rather than become a runtime error.
#[allow(
    clippy::too_many_lines,
    reason = "linear lifecycle order is the contract under test"
)]
pub fn run_provider_contract<P>(mut fixture: P) -> PlatformResult<ProviderContractReport>
where
    P: ProviderContractFixture,
{
    let volume = fixture.discover_volumes()?.remove(0);
    let mut initial = Vec::new();
    let summary = fixture.initial_scan(&volume, &mut |event| {
        initial.push(event);
        Ok(())
    })?;
    let encoded = serde_json::to_vec(&summary.checkpoint).map_err(|error| {
        crate::PlatformError::new(
            PlatformErrorKind::Internal,
            "serialize_checkpoint",
            error.to_string(),
        )
    })?;
    let checkpoint: ProviderCheckpoint = serde_json::from_slice(&encoded).map_err(|error| {
        crate::PlatformError::new(
            PlatformErrorKind::Internal,
            "deserialize_checkpoint",
            error.to_string(),
        )
    })?;
    assert_eq!(checkpoint, summary.checkpoint);
    assert_eq!(summary.emitted_events, initial.len() as u64);
    assert_eq!(initial, fixture.projected_state());
    let identity = fixture.primary_object();

    let mutations = [
        ContractMutation::Create,
        ContractMutation::Metadata,
        ContractMutation::Rename,
        ContractMutation::Move,
        ContractMutation::DirectoryRename,
        ContractMutation::HardLinkCreate,
        ContractMutation::HardLinkDelete,
        ContractMutation::Delete,
    ];
    let mut cursor = checkpoint;
    let mut created_object = None;
    for mutation in mutations {
        fixture.apply(mutation)?;
        let mut observed = Vec::new();
        let batch = fixture.read_changes(&cursor, 64, &mut |event| {
            observed.push(event);
            Ok(())
        })?;
        assert_eq!(
            usize::try_from(batch.emitted_events).expect("u32 fits usize"),
            observed.len()
        );
        assert_mutation_events(mutation, &observed, identity, &mut created_object);
        assert_projection_reflects(&observed, &fixture.projected_state());
        cursor = batch.checkpoint;
        if !matches!(
            mutation,
            ContractMutation::Delete | ContractMutation::Create
        ) {
            assert_eq!(
                fixture.primary_object(),
                identity,
                "object identity changed during {mutation:?}"
            );
        }
    }

    let restarted = fixture.restart();
    let resumed = restarted.read_changes(&cursor, 64, &mut |_| Ok(()))?;
    assert_eq!(resumed.emitted_events, 0);

    fixture.recreate_history();
    let gap = fixture
        .read_changes(&cursor, 64, &mut |_| Ok(()))
        .expect_err("history recreation must invalidate cursor");
    assert_eq!(gap.kind, PlatformErrorKind::SourceHistoryGap);

    fixture.set_online(false);
    let offline = fixture
        .initial_scan(&volume, &mut |_| Ok(()))
        .expect_err("offline volume must be explicit");
    assert_eq!(offline.kind, PlatformErrorKind::Unavailable);
    fixture.set_online(true);
    let mut reconciled = Vec::new();
    fixture.reconcile(&volume, &mut |event| {
        reconciled.push(event);
        Ok(())
    })?;
    assert_eq!(reconciled, fixture.projected_state());

    Ok(ProviderContractReport {
        assertions: 33,
        scenarios: vec![
            "stable_initial_observations",
            "opaque_checkpoint_serde",
            "create",
            "metadata",
            "rename",
            "move",
            "directory_rename",
            "delete",
            "hard_link_create_delete",
            "restart_resume",
            "journal_gap_recreation",
            "offline_online",
            "reconciliation_convergence",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    })
}

fn assert_mutation_events(
    mutation: ContractMutation,
    events: &[FilesystemEvent],
    primary: FileKey,
    created: &mut Option<FileKey>,
) {
    match mutation {
        ContractMutation::Create => match events {
            [
                FilesystemEvent::ObjectObserved { object },
                FilesystemEvent::LinkObserved { link },
            ] if object.object_key == link.object_key => {
                *created = Some(object.object_key);
            }
            _ => panic!("create must emit matching ObjectObserved + LinkObserved: {events:?}"),
        },
        ContractMutation::Metadata => assert!(
            matches!(events, [FilesystemEvent::ObjectObserved { object }] if object.object_key == primary)
        ),
        ContractMutation::Rename | ContractMutation::Move | ContractMutation::HardLinkCreate => {
            assert!(
                matches!(events, [FilesystemEvent::LinkObserved { link }] if link.object_key == primary)
            );
        }
        ContractMutation::DirectoryRename => {
            assert!(matches!(events, [FilesystemEvent::LinkObserved { .. }]));
        }
        ContractMutation::HardLinkDelete => assert!(
            matches!(events, [FilesystemEvent::LinkRemoved { object_key, .. }] if *object_key == primary)
        ),
        ContractMutation::Delete => {
            let expected = created.expect("create must precede delete");
            assert!(
                matches!(events, [FilesystemEvent::LinkRemoved { object_key: link_object, .. }, FilesystemEvent::ObjectRemoved { object_key }] if *link_object == expected && *object_key == expected)
            );
        }
    }
}

fn assert_projection_reflects(events: &[FilesystemEvent], projection: &[FilesystemEvent]) {
    for event in events {
        match event {
            FilesystemEvent::ObjectObserved { .. } => assert!(projection.contains(event), "object upsert missing from projection"),
            FilesystemEvent::LinkObserved { .. } => assert!(projection.contains(event), "link upsert missing from projection"),
            FilesystemEvent::LinkRemoved { file_link_id, .. } => assert!(!projection.iter().any(|candidate| matches!(candidate, FilesystemEvent::LinkObserved { link } if link.file_link_id == *file_link_id)), "removed link remains in projection"),
            FilesystemEvent::ObjectRemoved { object_key } => assert!(!projection.iter().any(|candidate| matches!(candidate, FilesystemEvent::ObjectObserved { object } if object.object_key == *object_key)), "removed object remains in projection"),
            FilesystemEvent::ReconciliationRequired { .. } => panic!("ordinary contract mutation must not request reconciliation"),
        }
    }
}
