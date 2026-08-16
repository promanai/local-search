use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, VolumeId,
};
use localsearch_platform_core::testing::{
    ContractMutation, ProviderContractFixture, run_provider_contract,
};
use localsearch_platform_core::{
    ChangeBatch, ChangeTrackingMode, FilesystemEventSink, FilesystemProvider, InitialScanMode,
    PlatformCapabilities, PlatformError, PlatformErrorKind, PlatformFamily, PlatformResult,
    PrivilegeModel, ProviderCheckpoint, ScanSummary, VolumeDescriptor,
};

#[derive(Clone)]
struct DeterministicWindowsFixture {
    volume: VolumeDescriptor,
    generation: u64,
    online: bool,
    object: FileKey,
    primary_link: FileLinkSnapshot,
    projections: Vec<FilesystemEvent>,
    history: Vec<FilesystemEvent>,
}

impl DeterministicWindowsFixture {
    fn new() -> Self {
        let volume_id = VolumeId::from_u128(1);
        let object = FileKey::new(volume_id, FileId128::from_u128(10));
        let primary_link = FileLinkSnapshot {
            file_link_id: FileLinkId::from_u128(11),
            object_key: object,
            parent_key: None,
            name: "alpha.txt".to_owned(),
        };
        let projections = vec![
            object_event(object, 10),
            FilesystemEvent::LinkObserved {
                link: primary_link.clone(),
            },
        ];
        Self {
            volume: VolumeDescriptor {
                volume_id,
                display_name: Some("contract-ntfs".to_owned()),
                mount_points: vec!["X:\\".to_owned()],
                filesystem: Some("NTFS".to_owned()),
                removable: false,
                local: true,
            },
            generation: 1,
            online: true,
            object,
            primary_link,
            projections,
            history: Vec::new(),
        }
    }
    fn checkpoint(&self, cursor: usize) -> PlatformResult<ProviderCheckpoint> {
        Ok(ProviderCheckpoint {
            provider_id: "fixture.windows-fs".to_owned(),
            format_version: 1,
            volume_id: self.volume.volume_id,
            opaque: serde_json::to_vec(&(self.generation, cursor)).map_err(|error| {
                PlatformError::new(
                    PlatformErrorKind::Internal,
                    "fixture_checkpoint",
                    error.to_string(),
                )
            })?,
        })
    }
    fn parse(&self, checkpoint: &ProviderCheckpoint) -> PlatformResult<(u64, usize)> {
        if checkpoint.provider_id != "fixture.windows-fs"
            || checkpoint.volume_id != self.volume.volume_id
        {
            return Err(PlatformError::new(
                PlatformErrorKind::InvalidCheckpoint,
                "fixture_checkpoint",
                "identity mismatch",
            ));
        }
        serde_json::from_slice(&checkpoint.opaque).map_err(|error| {
            PlatformError::new(
                PlatformErrorKind::InvalidCheckpoint,
                "fixture_checkpoint",
                error.to_string(),
            )
        })
    }
    fn push(&mut self, event: FilesystemEvent) {
        self.history.push(event);
    }
    fn replace_object(&mut self, event: FilesystemEvent) {
        self.projections.retain(|item| !matches!(item, FilesystemEvent::ObjectObserved { object } if object.object_key == self.object));
        self.projections.insert(0, event.clone());
        self.push(event);
    }
    fn replace_primary_link(&mut self) {
        self.projections.retain(|item| !matches!(item, FilesystemEvent::LinkObserved { link } if link.file_link_id == self.primary_link.file_link_id));
        let event = FilesystemEvent::LinkObserved {
            link: self.primary_link.clone(),
        };
        self.projections.push(event.clone());
        self.push(event);
    }
}

impl FilesystemProvider for DeterministicWindowsFixture {
    fn capabilities(&self) -> PlatformCapabilities {
        PlatformCapabilities {
            platform: PlatformFamily::Windows,
            initial_scan: InitialScanMode::FastMetadataEnumeration,
            change_tracking: ChangeTrackingMode::PersistentObjectJournal,
            privilege_model: PrivilegeModel::OptionalBroker,
            stable_object_ids: true,
            hard_links: true,
            persistent_history: true,
        }
    }
    fn discover_volumes(&self) -> PlatformResult<Vec<VolumeDescriptor>> {
        Ok(vec![self.volume.clone()])
    }
    fn initial_scan(
        &self,
        _volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        if !self.online {
            return Err(PlatformError::new(
                PlatformErrorKind::Unavailable,
                "fixture_scan",
                "offline",
            ));
        }
        for event in self.projections.clone() {
            sink.emit(event)?;
        }
        Ok(ScanSummary {
            checkpoint: self.checkpoint(self.history.len())?,
            emitted_events: self.projections.len() as u64,
        })
    }
    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        maximum_events: u32,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ChangeBatch> {
        let (generation, cursor) = self.parse(checkpoint)?;
        if generation != self.generation {
            return Err(PlatformError::new(
                PlatformErrorKind::SourceHistoryGap,
                "fixture_changes",
                "journal recreated",
            ));
        }
        let limit = usize::try_from(maximum_events).unwrap_or(usize::MAX);
        let end = cursor.saturating_add(limit).min(self.history.len());
        for event in &self.history[cursor..end] {
            sink.emit(event.clone())?;
        }
        Ok(ChangeBatch {
            checkpoint: self.checkpoint(end)?,
            emitted_events: u32::try_from(end - cursor).map_err(|_| {
                PlatformError::new(
                    PlatformErrorKind::Internal,
                    "fixture_changes",
                    "bounded event count overflow",
                )
            })?,
            has_more: end < self.history.len(),
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

impl ProviderContractFixture for DeterministicWindowsFixture {
    fn apply(&mut self, mutation: ContractMutation) -> PlatformResult<()> {
        match mutation {
            ContractMutation::Create => {
                let key = FileKey::new(self.volume.volume_id, FileId128::from_u128(20));
                let object = object_event(key, 1);
                let link = FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(21),
                        object_key: key,
                        parent_key: None,
                        name: "created.txt".to_owned(),
                    },
                };
                self.projections.extend([object.clone(), link.clone()]);
                self.push(object);
                self.push(link);
            }
            ContractMutation::Metadata => self.replace_object(object_event(self.object, 99)),
            ContractMutation::Rename => {
                "renamed.txt".clone_into(&mut self.primary_link.name);
                self.replace_primary_link();
            }
            ContractMutation::Move => {
                self.primary_link.parent_key = Some(FileKey::new(
                    self.volume.volume_id,
                    FileId128::from_u128(30),
                ));
                self.replace_primary_link();
            }
            ContractMutation::DirectoryRename => {
                let directory = FileKey::new(self.volume.volume_id, FileId128::from_u128(30));
                let event = FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(31),
                        object_key: directory,
                        parent_key: None,
                        name: "renamed-dir".to_owned(),
                    },
                };
                self.projections.retain(|item| !matches!(item, FilesystemEvent::LinkObserved { link } if link.file_link_id == FileLinkId::from_u128(31)));
                self.projections.push(event.clone());
                self.push(event);
            }
            ContractMutation::HardLinkCreate => {
                let event = FilesystemEvent::LinkObserved {
                    link: FileLinkSnapshot {
                        file_link_id: FileLinkId::from_u128(12),
                        object_key: self.object,
                        parent_key: None,
                        name: "hard.txt".to_owned(),
                    },
                };
                self.projections.push(event.clone());
                self.push(event);
            }
            ContractMutation::HardLinkDelete => {
                let id = FileLinkId::from_u128(12);
                self.projections.retain(|item| !matches!(item, FilesystemEvent::LinkObserved { link } if link.file_link_id == id));
                self.push(FilesystemEvent::LinkRemoved {
                    file_link_id: id,
                    object_key: self.object,
                });
            }
            ContractMutation::Delete => {
                let key = FileKey::new(self.volume.volume_id, FileId128::from_u128(20));
                let id = FileLinkId::from_u128(21);
                self.projections.retain(|item| !matches!(item, FilesystemEvent::ObjectObserved { object } if object.object_key == key) && !matches!(item, FilesystemEvent::LinkObserved { link } if link.file_link_id == id));
                self.push(FilesystemEvent::LinkRemoved {
                    file_link_id: id,
                    object_key: key,
                });
                self.push(FilesystemEvent::ObjectRemoved { object_key: key });
            }
        }
        Ok(())
    }
    fn restart(&self) -> Self {
        self.clone()
    }
    fn recreate_history(&mut self) {
        self.generation += 1;
        self.history.clear();
    }
    fn set_online(&mut self, online: bool) {
        self.online = online;
    }
    fn primary_object(&self) -> FileKey {
        self.object
    }
    fn projected_state(&self) -> Vec<FilesystemEvent> {
        self.projections.clone()
    }
}

fn object_event(key: FileKey, size: u64) -> FilesystemEvent {
    FilesystemEvent::ObjectObserved {
        object: FileObjectSnapshot {
            object_key: key,
            metadata: FileMetadata {
                kind: FileKind::File,
                size,
                created_at_unix_ms: None,
                modified_at_unix_ms: Some(1),
                hidden: false,
                availability: Availability::Online,
            },
        },
    }
}

#[test]
fn windows_semantics_pass_shared_provider_contract() -> PlatformResult<()> {
    let report = run_provider_contract(DeterministicWindowsFixture::new())?;
    assert_eq!(report.scenarios.len(), 13);
    Ok(())
}

#[cfg(windows)]
#[test]
fn live_provider_discovers_volumes_without_leaking_native_types() -> PlatformResult<()> {
    let provider = localsearch_windows_fs::WindowsFilesystemProvider::new();
    let volumes = provider.discover_volumes()?;
    assert!(!volumes.is_empty());
    assert!(volumes.iter().all(|volume| !volume.mount_points.is_empty()));
    Ok(())
}
