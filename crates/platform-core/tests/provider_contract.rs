use std::error::Error;

use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, VolumeId,
};
use localsearch_platform_core::{
    ChangeBatch, ChangeTrackingMode, FilesystemEventSink, FilesystemProvider, InitialScanMode,
    PlatformCapabilities, PlatformFamily, PlatformResult, PowerSource, PrivilegeModel,
    ProviderCheckpoint, ResourceProvider, ScanSummary, StorageClass, StorageResources,
    SystemResources, VolumeDescriptor,
};

#[derive(Clone, Copy)]
struct FixtureFilesystemProvider {
    capabilities: PlatformCapabilities,
}

impl FixtureFilesystemProvider {
    fn volume() -> VolumeDescriptor {
        VolumeDescriptor {
            volume_id: VolumeId::from_u128(1),
            display_name: Some("fixture".to_owned()),
            mount_points: vec!["/fixture".to_owned()],
            filesystem: Some("fixturefs".to_owned()),
            removable: false,
            local: true,
        }
    }

    fn checkpoint(volume_id: VolumeId) -> ProviderCheckpoint {
        ProviderCheckpoint {
            provider_id: "fixture-provider".to_owned(),
            format_version: 1,
            volume_id,
            opaque: vec![0x01, 0x02, 0x03],
        }
    }

    fn canonical_events(volume_id: VolumeId) -> [FilesystemEvent; 2] {
        let object_key = FileKey::new(volume_id, FileId128::from_u128(2));
        [
            FilesystemEvent::ObjectObserved {
                object: FileObjectSnapshot {
                    object_key,
                    metadata: FileMetadata {
                        kind: FileKind::File,
                        size: 42,
                        created_at_unix_ms: None,
                        modified_at_unix_ms: Some(1_786_662_000_000),
                        hidden: false,
                        availability: Availability::Online,
                    },
                },
            },
            FilesystemEvent::LinkObserved {
                link: FileLinkSnapshot {
                    file_link_id: FileLinkId::from_u128(3),
                    object_key,
                    parent_key: None,
                    name: "portable.txt".to_owned(),
                },
            },
        ]
    }
}

impl FilesystemProvider for FixtureFilesystemProvider {
    fn capabilities(&self) -> PlatformCapabilities {
        self.capabilities
    }

    fn discover_volumes(&self) -> PlatformResult<Vec<VolumeDescriptor>> {
        Ok(vec![Self::volume()])
    }

    fn initial_scan(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        for event in Self::canonical_events(volume.volume_id) {
            sink.emit(event)?;
        }
        Ok(ScanSummary {
            checkpoint: Self::checkpoint(volume.volume_id),
            emitted_events: 2,
        })
    }

    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        _maximum_events: u32,
        _sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ChangeBatch> {
        Ok(ChangeBatch {
            checkpoint: checkpoint.clone(),
            emitted_events: 0,
            has_more: false,
        })
    }

    fn reconcile(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        self.initial_scan(volume, sink)
    }
}

struct FixtureResourceProvider;

impl ResourceProvider for FixtureResourceProvider {
    fn snapshot(&self) -> PlatformResult<SystemResources> {
        Ok(SystemResources {
            logical_processors: 8,
            total_memory_bytes: Some(16 * 1024 * 1024 * 1024),
            available_memory_bytes: Some(8 * 1024 * 1024 * 1024),
            system_cpu_load_basis_points: Some(1_500),
            process_cpu_load_basis_points: Some(250),
            power_source: PowerSource::Ac,
            battery_percent: None,
            energy_saver: false,
            storage_busy_basis_points: Some(1_250),
            user_idle_duration_millis: Some(30_000),
            storage: vec![StorageResources {
                volume_id: VolumeId::from_u128(1),
                class: StorageClass::Nvme,
                capacity_bytes: Some(1_000_000_000_000),
                available_bytes: Some(500_000_000_000),
            }],
        })
    }
}

fn provider(platform: PlatformFamily) -> FixtureFilesystemProvider {
    let (initial_scan, change_tracking, privilege_model, stable_object_ids, persistent_history) =
        match platform {
            PlatformFamily::Windows => (
                InitialScanMode::FastMetadataEnumeration,
                ChangeTrackingMode::PersistentObjectJournal,
                PrivilegeModel::OptionalBroker,
                true,
                true,
            ),
            PlatformFamily::MacOs => (
                InitialScanMode::FilesystemCrawl,
                ChangeTrackingMode::PersistentPathStream,
                PrivilegeModel::UserConsent,
                false,
                true,
            ),
            PlatformFamily::Linux => (
                InitialScanMode::FilesystemCrawl,
                ChangeTrackingMode::EphemeralWatchers,
                PrivilegeModel::CurrentUser,
                false,
                false,
            ),
        };

    FixtureFilesystemProvider {
        capabilities: PlatformCapabilities {
            platform,
            initial_scan,
            change_tracking,
            privilege_model,
            stable_object_ids,
            hard_links: true,
            persistent_history,
        },
    }
}

#[test]
fn platform_capabilities_vary_but_canonical_events_do_not() -> PlatformResult<()> {
    let mut expected = None;

    for family in [
        PlatformFamily::Windows,
        PlatformFamily::MacOs,
        PlatformFamily::Linux,
    ] {
        let provider = provider(family);
        let volume = provider.discover_volumes()?.remove(0);
        let mut events = Vec::new();
        let mut sink = |event| {
            events.push(event);
            Ok(())
        };
        let summary = provider.initial_scan(&volume, &mut sink)?;

        assert_eq!(provider.capabilities().platform, family);
        assert_eq!(summary.emitted_events, 2);
        if let Some(expected) = &expected {
            assert_eq!(&events, expected);
        } else {
            expected = Some(events);
        }
    }

    Ok(())
}

#[test]
fn provider_checkpoint_is_opaque_and_round_trips() -> Result<(), Box<dyn Error>> {
    let checkpoint = FixtureFilesystemProvider::checkpoint(VolumeId::from_u128(1));
    let encoded = serde_json::to_string(&checkpoint)?;
    let decoded = serde_json::from_str::<ProviderCheckpoint>(&encoded)?;

    assert_eq!(decoded, checkpoint);
    assert!(!encoded.contains("usn"));
    assert!(!encoded.contains("fsevent"));
    assert!(!encoded.contains("inotify"));

    Ok(())
}

#[test]
fn resource_provider_returns_portable_policy_inputs() -> PlatformResult<()> {
    let snapshot = FixtureResourceProvider.snapshot()?;

    assert_eq!(snapshot.logical_processors, 8);
    assert_eq!(snapshot.system_cpu_load_basis_points, Some(1_500));
    assert_eq!(snapshot.storage_busy_basis_points, Some(1_250));
    assert_eq!(snapshot.user_idle_duration_millis, Some(30_000));
    assert_eq!(snapshot.storage[0].class, StorageClass::Nvme);

    Ok(())
}
