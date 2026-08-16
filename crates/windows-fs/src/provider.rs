use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};

use localsearch_core::{
    Availability, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, ReconciliationReason, VolumeId,
};
use localsearch_platform_core::{
    ChangeBatch, ChangeTrackingMode, FilesystemEventSink, FilesystemProvider, InitialScanMode,
    PlatformCapabilities, PlatformError, PlatformErrorKind, PlatformFamily, PlatformResult,
    PrivilegeModel, ProviderCheckpoint, ScanSummary, VolumeDescriptor,
};

use crate::checkpoint::{
    KnownLinkIdentity, PendingRename, WindowsCheckpoint, decode, encode, validate,
};
use crate::{
    journal::{JournalSession, JournalState},
    record::{
        REASON_BASIC_INFO_CHANGE, REASON_DATA_CHANGE, REASON_FILE_CREATE, REASON_FILE_DELETE,
        REASON_HARD_LINK_CHANGE, REASON_RENAME_NEW_NAME, REASON_RENAME_OLD_NAME,
        SanitizedUsnRecord,
    },
};

#[derive(Clone, Debug)]
struct NativeVolume {
    descriptor: VolumeDescriptor,
    root: PathBuf,
}

/// Windows provider spike with real volume discovery and a non-following safe enumeration path.
pub struct WindowsFilesystemProvider {
    volumes: RwLock<HashMap<VolumeId, NativeVolume>>,
    tracking: TrackingMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrackingMode {
    Snapshot,
    UsnJournal,
}

enum ObjectLookup {
    Observed(FileObjectSnapshot),
    Missing,
    Unavailable,
}

/// Completed current-user snapshot of one explicitly selected directory tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedScanSummary {
    /// Synthetic source descriptor unique to the selected canonical root.
    pub volume: VolumeDescriptor,
    /// Opaque snapshot checkpoint and emitted event count.
    pub scan: ScanSummary,
}

/// Canonical scoped root prepared before a potentially large streaming crawl.
///
/// Separating preparation from traversal lets durable consumers commit bounded event batches
/// without changing the root-derived identity used by later reconciliation passes.
#[derive(Clone, Debug)]
pub struct PreparedScopedScan {
    canonical: PathBuf,
    volume: VolumeDescriptor,
    root_key: FileKey,
}

/// Traversal policy for one prepared scoped scan.
#[derive(Clone, Debug, Default)]
pub struct ScopedScanOptions {
    excluded_directory_names: BTreeSet<String>,
}

impl ScopedScanOptions {
    /// Creates a case-insensitive directory-name exclusion policy.
    #[must_use]
    pub fn excluding(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            excluded_directory_names: names
                .into_iter()
                .map(|name| name.into().to_lowercase())
                .collect(),
        }
    }

    fn descends_into(&self, name: &str) -> bool {
        !self.excluded_directory_names.contains(&name.to_lowercase())
    }
}

impl PreparedScopedScan {
    /// Returns the synthetic source descriptor stable for this physical volume and canonical root.
    #[must_use]
    pub const fn volume(&self) -> &VolumeDescriptor {
        &self.volume
    }
}

impl WindowsFilesystemProvider {
    /// Creates an empty provider. Volume discovery refreshes its private native-volume table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            volumes: RwLock::new(HashMap::new()),
            tracking: TrackingMode::Snapshot,
        }
    }

    /// Creates a provider that requires access to the durable NTFS USN journal.
    ///
    /// Use this mode in an elevated broker. Permission and journal availability failures remain
    /// typed platform errors; they never silently downgrade a requested durable stream.
    #[must_use]
    pub fn new_with_usn_journal() -> Self {
        Self {
            volumes: RwLock::new(HashMap::new()),
            tracking: TrackingMode::UsnJournal,
        }
    }

    fn native_volume(&self, id: VolumeId) -> PlatformResult<NativeVolume> {
        self.volumes
            .read()
            .map_err(|_| internal("volume table poisoned"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| {
                PlatformError::new(
                    PlatformErrorKind::Unavailable,
                    "resolve_volume",
                    "volume is not currently online",
                )
            })
    }

    /// Scans one explicitly selected directory without enumerating sibling trees on its volume.
    ///
    /// Native object identities remain stable across file renames. The selected canonical path is
    /// folded into a scoped source identity so multiple roots on one physical volume cannot
    /// collide in the durable graph. Reparse-point directories are observed but never traversed.
    ///
    /// # Errors
    ///
    /// Returns a categorized platform error when the root is unavailable, is not a directory, is
    /// outside a discovered local volume, or cannot be enumerated with current-user permissions.
    pub fn initial_scan_root(
        &self,
        root: &Path,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScopedScanSummary> {
        let prepared = self.prepare_scan_root(root)?;
        self.scan_prepared_root(&prepared, sink)
    }

    /// Resolves and validates one explicit root without traversing its descendants.
    ///
    /// # Errors
    ///
    /// Returns a categorized platform error when the root is unavailable, is not a directory, or
    /// is outside a discovered local volume.
    pub fn prepare_scan_root(&self, root: &Path) -> PlatformResult<PreparedScopedScan> {
        let canonical =
            fs::canonicalize(root).map_err(|error| io_error("canonicalize_root", &error))?;
        if !canonical.is_dir() {
            return Err(PlatformError::new(
                PlatformErrorKind::Unsupported,
                "scan_root",
                "selected root is not a directory",
            ));
        }
        let native = native::discover()?
            .into_iter()
            .filter_map(|volume| {
                let mount = fs::canonicalize(&volume.root).ok()?;
                canonical.starts_with(&mount).then_some((volume, mount))
            })
            .max_by_key(|(_, mount)| mount.as_os_str().len())
            .map(|(volume, _)| volume)
            .ok_or_else(|| {
                PlatformError::new(
                    PlatformErrorKind::Unavailable,
                    "scan_root",
                    "selected root is outside a discovered local volume",
                )
            })?;
        let volume_id = scoped_volume_id(native.descriptor.volume_id, &canonical);
        let display_root = display_root_path(&canonical);
        let descriptor = VolumeDescriptor {
            volume_id,
            display_name: Some(display_root.to_string_lossy().into_owned()),
            mount_points: vec![display_root.to_string_lossy().into_owned()],
            filesystem: native.descriptor.filesystem,
            removable: native.descriptor.removable,
            local: native.descriptor.local,
        };
        let root_key = file_key(volume_id, native::file_identity(&canonical)?);
        Ok(PreparedScopedScan {
            canonical,
            volume: descriptor,
            root_key,
        })
    }

    /// Traverses a prepared root and streams canonical observations to `sink`.
    ///
    /// # Errors
    ///
    /// Returns a categorized platform or sink error. Reparse-point directories are observed but
    /// never traversed.
    pub fn scan_prepared_root(
        &self,
        prepared: &PreparedScopedScan,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScopedScanSummary> {
        self.scan_prepared_root_with_options(prepared, &ScopedScanOptions::default(), sink)
    }

    /// Traverses a prepared root under an explicit directory exclusion policy.
    ///
    /// Excluded directories themselves remain observable/searchable metadata, but their
    /// descendants are not traversed or content-indexed.
    ///
    /// # Errors
    ///
    /// Returns a categorized platform or sink error.
    pub fn scan_prepared_root_with_options(
        &self,
        prepared: &PreparedScopedScan,
        options: &ScopedScanOptions,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScopedScanSummary> {
        let canonical = &prepared.canonical;
        let descriptor = prepared.volume.clone();
        let volume_id = descriptor.volume_id;
        let root_key = prepared.root_key;
        let display_root = display_root_path(canonical);
        let ObjectLookup::Observed(root_object) = object_snapshot(canonical, root_key)? else {
            return Err(PlatformError::new(
                PlatformErrorKind::Unavailable,
                "scan_root",
                "selected root metadata is unavailable",
            ));
        };
        sink.emit(FilesystemEvent::ObjectObserved {
            object: root_object,
        })?;
        sink.emit(FilesystemEvent::LinkObserved {
            link: FileLinkSnapshot {
                file_link_id: root_link_id(volume_id),
                object_key: root_key,
                parent_key: None,
                name: display_root.to_string_lossy().into_owned(),
            },
        })?;
        let mut emitted = 2_u64;
        crawl(
            canonical,
            volume_id,
            Some(root_key),
            options,
            sink,
            &mut emitted,
        )?;
        let root_fingerprint = scoped_root_fingerprint(canonical);
        Ok(ScopedScanSummary {
            volume: descriptor,
            scan: ScanSummary {
                checkpoint: ProviderCheckpoint {
                    provider_id: "windows-scoped-folder".to_owned(),
                    format_version: 1,
                    volume_id,
                    opaque: root_fingerprint.to_be_bytes().to_vec(),
                },
                emitted_events: emitted,
            },
        })
    }
}

impl Default for WindowsFilesystemProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesystemProvider for WindowsFilesystemProvider {
    fn capabilities(&self) -> PlatformCapabilities {
        let journal = self.tracking == TrackingMode::UsnJournal;
        PlatformCapabilities {
            platform: PlatformFamily::Windows,
            initial_scan: InitialScanMode::FilesystemCrawl,
            change_tracking: if journal {
                ChangeTrackingMode::PersistentObjectJournal
            } else {
                ChangeTrackingMode::SnapshotOnly
            },
            privilege_model: PrivilegeModel::OptionalBroker,
            stable_object_ids: true,
            hard_links: true,
            persistent_history: journal,
        }
    }

    fn discover_volumes(&self) -> PlatformResult<Vec<VolumeDescriptor>> {
        let discovered = native::discover()?;
        let descriptors = discovered
            .iter()
            .map(|volume| volume.descriptor.clone())
            .collect();
        *self
            .volumes
            .write()
            .map_err(|_| internal("volume table poisoned"))? = discovered
            .into_iter()
            .map(|volume| (volume.descriptor.volume_id, volume))
            .collect();
        Ok(descriptors)
    }

    fn initial_scan(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        if !volume
            .filesystem
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("ntfs"))
        {
            return Err(PlatformError::new(
                PlatformErrorKind::Unsupported,
                "initial_scan",
                "START-004 native path supports NTFS only",
            ));
        }
        let native = self.native_volume(volume.volume_id)?;
        let journal = if self.tracking == TrackingMode::UsnJournal {
            let session = JournalSession::open(&native.root)?;
            let state = session.query()?;
            Some((session, state))
        } else {
            None
        };
        let mut emitted = 0_u64;
        let root_reference = native::file_identity(&native.root)?;
        let root_key = file_key(volume.volume_id, root_reference);
        let ObjectLookup::Observed(root) = object_snapshot(&native.root, root_key)? else {
            return Err(PlatformError::new(
                PlatformErrorKind::Unavailable,
                "initial_scan",
                "volume root metadata is unavailable",
            ));
        };
        sink.emit(FilesystemEvent::ObjectObserved { object: root })?;
        emitted += 1;
        let root_name = native
            .root
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned();
        sink.emit(FilesystemEvent::LinkObserved {
            link: FileLinkSnapshot {
                file_link_id: root_link_id(volume.volume_id),
                object_key: root_key,
                parent_key: None,
                name: root_name,
            },
        })?;
        emitted += 1;
        crawl(
            &native.root,
            volume.volume_id,
            Some(root_key),
            &ScopedScanOptions::default(),
            sink,
            &mut emitted,
        )?;
        let checkpoint = if let Some((session, before)) = journal {
            let after = session.query()?;
            let saved = WindowsCheckpoint {
                journal_identity: before.identity,
                next_position: before.next_position,
                lowest_valid_position: retained_start(before),
                snapshot_generation: 1,
                pending_events: Vec::new(),
                pending_renames: Vec::new(),
                known_links: Vec::new(),
            };
            validate(&saved, after.identity, retained_start(after))?;
            saved
        } else {
            WindowsCheckpoint {
                journal_identity: checkpoint_identity(volume.volume_id),
                next_position: 0,
                lowest_valid_position: 0,
                snapshot_generation: 1,
                pending_events: Vec::new(),
                pending_renames: Vec::new(),
                known_links: Vec::new(),
            }
        };
        Ok(ScanSummary {
            checkpoint: encode(volume.volume_id, &checkpoint)?,
            emitted_events: emitted,
        })
    }

    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        maximum_events: u32,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ChangeBatch> {
        let mut saved = decode(checkpoint)?;
        if self.tracking != TrackingMode::UsnJournal {
            return Err(PlatformError::new(
                PlatformErrorKind::Unsupported,
                "read_changes",
                "this provider instance is configured for snapshots only",
            ));
        }
        let native = self.native_volume(checkpoint.volume_id)?;
        let session = JournalSession::open(&native.root)?;
        let before = session.query()?;
        validate(&saved, before.identity, retained_start(before))?;
        saved.lowest_valid_position = retained_start(before);

        let mut emitted = 0_u32;
        emit_pending(&mut saved, maximum_events, sink, &mut emitted)?;
        if !saved.pending_events.is_empty() || emitted == maximum_events {
            let has_more =
                !saved.pending_events.is_empty() || saved.next_position < before.next_position;
            return change_batch(checkpoint.volume_id, &saved, emitted, has_more);
        }

        let page = session.read(saved.next_position, saved.journal_identity)?;
        if page.next_position < saved.next_position {
            return Err(PlatformError::new(
                PlatformErrorKind::Io,
                "read_changes",
                "journal cursor moved backwards",
            ));
        }
        for (index, record) in page.records.iter().enumerate() {
            let next_position = page
                .records
                .get(index + 1)
                .map_or(page.next_position, |next| next.position);
            let events = canonical_events(&session, &mut saved, checkpoint.volume_id, record)?;
            let available = usize::try_from(maximum_events - emitted).map_err(|_| {
                PlatformError::new(
                    PlatformErrorKind::Internal,
                    "read_changes",
                    "event budget overflow",
                )
            })?;
            let accepted = available.min(events.len());
            for event in &events[..accepted] {
                sink.emit(event.clone())?;
                emitted += 1;
            }
            saved.next_position = next_position;
            if accepted < events.len() {
                saved.pending_events = events[accepted..].to_vec();
                return change_batch(checkpoint.volume_id, &saved, emitted, true);
            }
            if emitted == maximum_events {
                let has_more = saved.next_position < before.next_position;
                return change_batch(checkpoint.volume_id, &saved, emitted, has_more);
            }
        }
        let after = session.query()?;
        validate(&saved, after.identity, retained_start(after))?;
        saved.lowest_valid_position = retained_start(after);
        let has_more = saved.next_position < after.next_position;
        change_batch(checkpoint.volume_id, &saved, emitted, has_more)
    }

    fn reconcile(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        self.initial_scan(volume, sink)
    }
}

fn retained_start(state: JournalState) -> i64 {
    state.first_position.max(state.lowest_valid_position)
}

fn emit_pending(
    saved: &mut WindowsCheckpoint,
    maximum_events: u32,
    sink: &mut dyn FilesystemEventSink,
    emitted: &mut u32,
) -> PlatformResult<()> {
    let accepted = usize::try_from(maximum_events)
        .unwrap_or(usize::MAX)
        .min(saved.pending_events.len());
    for event in &saved.pending_events[..accepted] {
        sink.emit(event.clone())?;
        *emitted += 1;
    }
    saved.pending_events.drain(..accepted);
    Ok(())
}

fn change_batch(
    volume_id: VolumeId,
    saved: &WindowsCheckpoint,
    emitted_events: u32,
    has_more: bool,
) -> PlatformResult<ChangeBatch> {
    Ok(ChangeBatch {
        checkpoint: encode(volume_id, saved)?,
        emitted_events,
        has_more,
    })
}

fn canonical_events(
    session: &JournalSession,
    saved: &mut WindowsCheckpoint,
    volume_id: VolumeId,
    record: &SanitizedUsnRecord,
) -> PlatformResult<Vec<FilesystemEvent>> {
    if record.had_invalid_utf16 {
        return Ok(vec![FilesystemEvent::ReconciliationRequired {
            volume_id,
            reason: ReconciliationReason::ProviderRequested,
        }]);
    }
    let object_key = file_key(volume_id, record.file_reference);
    let parent_key = file_key(volume_id, record.parent_reference);
    let path_link_id = known_link_id(saved, record.parent_reference, &record.name)
        .unwrap_or_else(|| FileLinkId::from_u128(link_hash(Some(parent_key), &record.name)));
    if record.reason & REASON_RENAME_OLD_NAME != 0 {
        forget_known_link(saved, record.parent_reference, &record.name);
        remember_rename(saved, record.file_reference, path_link_id);
        return Ok(Vec::new());
    }
    let file_link_id = if record.reason & REASON_RENAME_NEW_NAME != 0 {
        let stable = take_rename(saved, record.file_reference).unwrap_or(path_link_id);
        remember_known_link(saved, record.parent_reference, &record.name, stable);
        stable
    } else {
        path_link_id
    };
    let link = FileLinkSnapshot {
        file_link_id,
        object_key,
        parent_key: Some(parent_key),
        name: record.name.clone(),
    };

    if record.reason & REASON_FILE_DELETE != 0 {
        forget_known_link(saved, record.parent_reference, &record.name);
        return Ok(vec![
            FilesystemEvent::LinkRemoved {
                file_link_id,
                object_key,
            },
            FilesystemEvent::ObjectRemoved { object_key },
        ]);
    }
    let lookup = match session.resolve_child(record.parent_reference, &record.name) {
        Ok(path) => object_snapshot(&path, object_key)?,
        Err(error)
            if matches!(
                error.kind,
                PlatformErrorKind::PermissionDenied | PlatformErrorKind::Unavailable
            ) =>
        {
            ObjectLookup::Unavailable
        }
        Err(error) => return Err(error),
    };

    if matches!(&lookup, ObjectLookup::Unavailable) {
        return Ok(Vec::new());
    }

    if record.reason & REASON_HARD_LINK_CHANGE != 0
        && record.reason & (REASON_FILE_CREATE | REASON_RENAME_NEW_NAME) == 0
    {
        return Ok(match lookup {
            ObjectLookup::Observed(_) => vec![FilesystemEvent::LinkObserved { link }],
            ObjectLookup::Missing => vec![FilesystemEvent::LinkRemoved {
                file_link_id,
                object_key,
            }],
            ObjectLookup::Unavailable => Vec::new(),
        });
    }

    let observes_link = record.reason & (REASON_FILE_CREATE | REASON_RENAME_NEW_NAME) != 0;
    let observes_object =
        record.reason & (REASON_FILE_CREATE | REASON_BASIC_INFO_CHANGE | REASON_DATA_CHANGE) != 0;
    let mut events = Vec::with_capacity(2);
    if observes_object && let ObjectLookup::Observed(object) = lookup {
        events.push(FilesystemEvent::ObjectObserved { object });
    }
    if observes_link {
        events.push(FilesystemEvent::LinkObserved { link });
    }
    Ok(events)
}

fn remember_rename(saved: &mut WindowsCheckpoint, file_reference: u64, file_link_id: FileLinkId) {
    match saved
        .pending_renames
        .binary_search_by_key(&file_reference, |pending| pending.file_reference)
    {
        Ok(index) => saved.pending_renames[index].file_link_id = file_link_id,
        Err(index) => saved.pending_renames.insert(
            index,
            PendingRename {
                file_reference,
                file_link_id,
            },
        ),
    }
}

fn take_rename(saved: &mut WindowsCheckpoint, file_reference: u64) -> Option<FileLinkId> {
    let index = saved
        .pending_renames
        .binary_search_by_key(&file_reference, |pending| pending.file_reference)
        .ok()?;
    Some(saved.pending_renames.remove(index).file_link_id)
}

fn known_link_id(
    saved: &WindowsCheckpoint,
    parent_reference: u64,
    name: &str,
) -> Option<FileLinkId> {
    saved
        .known_links
        .binary_search_by(|known| {
            (known.parent_reference, known.name.as_str()).cmp(&(parent_reference, name))
        })
        .ok()
        .map(|index| saved.known_links[index].file_link_id)
}

fn remember_known_link(
    saved: &mut WindowsCheckpoint,
    parent_reference: u64,
    name: &str,
    file_link_id: FileLinkId,
) {
    match saved.known_links.binary_search_by(|known| {
        (known.parent_reference, known.name.as_str()).cmp(&(parent_reference, name))
    }) {
        Ok(index) => saved.known_links[index].file_link_id = file_link_id,
        Err(index) => saved.known_links.insert(
            index,
            KnownLinkIdentity {
                parent_reference,
                name: name.to_owned(),
                file_link_id,
            },
        ),
    }
}

fn forget_known_link(saved: &mut WindowsCheckpoint, parent_reference: u64, name: &str) {
    if let Ok(index) = saved.known_links.binary_search_by(|known| {
        (known.parent_reference, known.name.as_str()).cmp(&(parent_reference, name))
    }) {
        saved.known_links.remove(index);
    }
}

fn file_key(volume_id: VolumeId, reference: u64) -> FileKey {
    FileKey::new(volume_id, FileId128::from_u128(u128::from(reference)))
}

fn root_link_id(volume_id: VolumeId) -> FileLinkId {
    FileLinkId::from_u128(volume_id.as_u128() ^ 0x006c_732d_726f_6f74_2d6c_696e_6b2d_7631)
}

#[cfg(windows)]
fn object_snapshot(path: &Path, object_key: FileKey) -> PlatformResult<ObjectLookup> {
    use std::os::windows::fs::MetadataExt;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ObjectLookup::Missing);
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Ok(ObjectLookup::Unavailable);
        }
        Err(error) => return Err(io_error("read_changed_metadata", &error)),
    };
    let attributes = metadata.file_attributes();
    let kind = if metadata.is_dir() {
        FileKind::Directory
    } else if metadata.file_type().is_symlink() {
        FileKind::Symlink
    } else if metadata.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    };
    Ok(ObjectLookup::Observed(FileObjectSnapshot {
        object_key,
        metadata: FileMetadata {
            kind,
            size: metadata.file_size(),
            created_at_unix_ms: filetime_ms(metadata.creation_time()),
            modified_at_unix_ms: filetime_ms(metadata.last_write_time()),
            hidden: attributes & 0x2 != 0,
            availability: Availability::Online,
        },
    }))
}

#[cfg(not(windows))]
fn object_snapshot(_path: &Path, _object_key: FileKey) -> PlatformResult<ObjectLookup> {
    Err(PlatformError::new(
        PlatformErrorKind::Unsupported,
        "read_changed_metadata",
        "Windows metadata requires Windows",
    ))
}

#[cfg(windows)]
fn readable_directory(path: &Path, descendant: bool) -> PlatformResult<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error)
            if descendant
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
        {
            Ok(None)
        }
        Err(error) => Err(io_error("read_directory", &error)),
    }
}

#[cfg(windows)]
fn crawl(
    path: &Path,
    volume_id: VolumeId,
    parent: Option<FileKey>,
    options: &ScopedScanOptions,
    sink: &mut dyn FilesystemEventSink,
    emitted: &mut u64,
) -> PlatformResult<()> {
    use std::os::windows::fs::MetadataExt;
    let Some(entries) = readable_directory(path, parent.is_some())? else {
        return Ok(());
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(io_error("read_directory_entry", &error)),
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(io_error("read_metadata", &error)),
        };
        let index = match native::file_identity(&entry.path()) {
            Ok(index) => index,
            Err(error)
                if matches!(
                    error.kind,
                    PlatformErrorKind::PermissionDenied | PlatformErrorKind::Unavailable
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let object_key = FileKey::new(volume_id, FileId128::from_u128(u128::from(index)));
        let attributes = metadata.file_attributes();
        let kind = if metadata.is_dir() {
            FileKind::Directory
        } else if metadata.file_type().is_symlink() {
            FileKind::Symlink
        } else if metadata.is_file() {
            FileKind::File
        } else {
            FileKind::Other
        };
        sink.emit(FilesystemEvent::ObjectObserved {
            object: FileObjectSnapshot {
                object_key,
                metadata: FileMetadata {
                    kind,
                    size: metadata.file_size(),
                    created_at_unix_ms: filetime_ms(metadata.creation_time()),
                    modified_at_unix_ms: filetime_ms(metadata.last_write_time()),
                    hidden: attributes & 0x2 != 0,
                    availability: Availability::Online,
                },
            },
        })?;
        *emitted = emitted
            .checked_add(1)
            .ok_or_else(|| internal("event count overflow"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let descends = options.descends_into(&name);
        sink.emit(FilesystemEvent::LinkObserved {
            link: FileLinkSnapshot {
                file_link_id: FileLinkId::from_u128(link_hash(parent, &name)),
                object_key,
                parent_key: parent,
                name,
            },
        })?;
        *emitted = emitted
            .checked_add(1)
            .ok_or_else(|| internal("event count overflow"))?;
        if kind == FileKind::Directory && attributes & 0x400 == 0 && descends {
            crawl(
                &entry.path(),
                volume_id,
                Some(object_key),
                options,
                sink,
                emitted,
            )?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn crawl(
    _path: &Path,
    _volume_id: VolumeId,
    _parent: Option<FileKey>,
    _options: &ScopedScanOptions,
    _sink: &mut dyn FilesystemEventSink,
    _emitted: &mut u64,
) -> PlatformResult<()> {
    Err(PlatformError::new(
        PlatformErrorKind::Unsupported,
        "initial_scan",
        "Windows provider requires Windows",
    ))
}

#[cfg(windows)]
fn filetime_ms(value: u64) -> Option<i64> {
    const EPOCH: u64 = 116_444_736_000_000_000;
    value
        .checked_sub(EPOCH)
        .and_then(|ticks| i64::try_from(ticks / 10_000).ok())
}
fn stable_hash_bytes(bytes: &[u8]) -> u128 {
    bytes.iter().fold(
        0x6c62_272e_07bb_0142_62b8_2175_6295_c58d_u128,
        |hash, byte| {
            (hash ^ u128::from(*byte)).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b)
        },
    )
}

fn scoped_root_fingerprint(root: &Path) -> u128 {
    stable_hash_bytes(root.to_string_lossy().to_lowercase().as_bytes())
}

fn display_root_path(canonical: &Path) -> PathBuf {
    let value = canonical.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    value
        .strip_prefix(r"\\?\")
        .map_or_else(|| canonical.to_path_buf(), PathBuf::from)
}

fn scoped_volume_id(source: VolumeId, root: &Path) -> VolumeId {
    let mut bytes = source.as_bytes().to_vec();
    bytes.extend_from_slice(&scoped_root_fingerprint(root).to_be_bytes());
    VolumeId::from_u128(stable_hash_bytes(&bytes))
}

fn volume_id_from_guid(guid: &str) -> VolumeId {
    VolumeId::from_u128(stable_hash_bytes(guid.as_bytes()))
}

fn link_hash(parent: Option<FileKey>, name: &str) -> u128 {
    let mut bytes = Vec::with_capacity(48 + name.len());
    if let Some(parent) = parent {
        bytes.extend_from_slice(parent.volume_id.as_bytes());
        bytes.extend_from_slice(parent.file_id.as_bytes());
    }
    bytes.extend_from_slice(name.as_bytes());
    stable_hash_bytes(&bytes)
}
fn checkpoint_identity(volume_id: VolumeId) -> u64 {
    let bytes = volume_id.into_bytes();
    u64::from_be_bytes(bytes[8..16].try_into().expect("fixed identifier slice"))
}

fn io_error(operation: &'static str, error: &std::io::Error) -> PlatformError {
    let kind = match error.kind() {
        std::io::ErrorKind::PermissionDenied => PlatformErrorKind::PermissionDenied,
        std::io::ErrorKind::NotFound => PlatformErrorKind::Unavailable,
        _ => PlatformErrorKind::Io,
    };
    PlatformError::new(kind, operation, error.to_string())
}
fn internal(detail: &'static str) -> PlatformError {
    PlatformError::new(PlatformErrorKind::Internal, "windows_provider", detail)
}

#[cfg(windows)]
mod native {
    #![allow(
        unsafe_code,
        reason = "audited leaf FFI calls copy Win32 outputs into owned Rust values"
    )]
    use super::{
        NativeVolume, Path, PathBuf, PlatformResult, VolumeDescriptor, io_error,
        volume_id_from_guid,
    };
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GetFileInformationByHandle, GetLogicalDrives, GetVolumeInformationW,
            GetVolumeNameForVolumeMountPointW, OPEN_EXISTING,
        },
    };

    struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: this wrapper is constructed only for one valid owned handle and closes once.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn file_identity(path: &Path) -> PlatformResult<u64> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // SAFETY: the path is NUL terminated; optional pointers are null; returned ownership is checked.
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io_error(
                "open_file_identity",
                &std::io::Error::last_os_error(),
            ));
        }
        let handle = OwnedHandle(raw);
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: handle is live and info points to writable initialized storage for the call.
        if unsafe { GetFileInformationByHandle(handle.0, &raw mut info) } == 0 {
            return Err(io_error(
                "read_file_identity",
                &std::io::Error::last_os_error(),
            ));
        }
        Ok((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
    }

    pub(super) fn discover() -> PlatformResult<Vec<NativeVolume>> {
        // SAFETY: GetLogicalDrives has no pointer arguments or lifetime requirements.
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Err(io_error(
                "discover_volumes",
                &std::io::Error::last_os_error(),
            ));
        }
        let mut volumes = Vec::new();
        for index in 0_u8..26 {
            if mask & (1_u32 << index) == 0 {
                continue;
            }
            let letter = char::from(b'A' + index);
            let root = format!("{letter}:\\");
            let wide = root.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let mut label = [0_u16; 261];
            let mut fs_name = [0_u16; 64];
            let mut volume_guid = [0_u16; 64];
            let mut serial = 0_u32;
            // SAFETY: all pointers refer to writable fixed arrays for the supplied lengths; input is NUL terminated.
            let ok = unsafe {
                GetVolumeInformationW(
                    wide.as_ptr(),
                    label.as_mut_ptr(),
                    261,
                    &raw mut serial,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs_name.as_mut_ptr(),
                    64,
                )
            };
            if ok == 0 {
                continue;
            }
            // SAFETY: input is NUL terminated and output points to a writable fixed buffer.
            if unsafe {
                GetVolumeNameForVolumeMountPointW(wide.as_ptr(), volume_guid.as_mut_ptr(), 64)
            } == 0
            {
                continue;
            }
            let fs = utf16_z(&fs_name);
            let display = utf16_z(&label);
            let guid = utf16_z(&volume_guid);
            let id = volume_id_from_guid(&guid);
            let descriptor = VolumeDescriptor {
                volume_id: id,
                display_name: (!display.is_empty()).then_some(display),
                mount_points: vec![root.clone()],
                filesystem: (!fs.is_empty()).then_some(fs),
                removable: false,
                local: true,
            };
            volumes.push(NativeVolume {
                descriptor,
                root: PathBuf::from(root),
            });
        }
        Ok(volumes)
    }
    fn utf16_z(value: &[u16]) -> String {
        let end = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        String::from_utf16_lossy(&value[..end])
    }
}

#[cfg(not(windows))]
mod native {
    use super::{NativeVolume, PlatformError, PlatformErrorKind, PlatformResult};
    pub(super) fn discover() -> PlatformResult<Vec<NativeVolume>> {
        Err(PlatformError::new(
            PlatformErrorKind::Unsupported,
            "discover_volumes",
            "Windows provider requires Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChangeTrackingMode, FilesystemProvider, ScopedScanOptions, WindowsFilesystemProvider,
        known_link_id, remember_known_link, remember_rename, take_rename, volume_id_from_guid,
    };
    use crate::checkpoint::WindowsCheckpoint;
    use localsearch_core::FileLinkId;

    #[cfg(windows)]
    use localsearch_core::{FileKind, FilesystemEvent};

    #[test]
    fn capabilities_do_not_claim_an_unimplemented_journal() {
        let capabilities = WindowsFilesystemProvider::new().capabilities();
        assert_eq!(
            capabilities.change_tracking,
            ChangeTrackingMode::SnapshotOnly
        );
        assert!(!capabilities.persistent_history);
    }

    #[test]
    fn explicit_journal_mode_advertises_persistent_object_history() {
        let capabilities = WindowsFilesystemProvider::new_with_usn_journal().capabilities();
        assert_eq!(
            capabilities.change_tracking,
            ChangeTrackingMode::PersistentObjectJournal
        );
        assert!(capabilities.persistent_history);
    }

    #[test]
    fn volume_identity_depends_on_volume_guid_not_mount_path() {
        let guid = r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\";
        let mounted_as_c = volume_id_from_guid(guid);
        let mounted_as_x = volume_id_from_guid(guid);
        assert_eq!(mounted_as_c, mounted_as_x);
        assert_ne!(
            mounted_as_c,
            volume_id_from_guid(r"\\?\Volume{ffffffff-89ab-cdef-0123-456789abcdef}\")
        );
    }

    #[test]
    fn rename_identity_survives_rename_move_and_later_delete_lookup() {
        let mut checkpoint = WindowsCheckpoint {
            journal_identity: 1,
            next_position: 0,
            lowest_valid_position: 0,
            snapshot_generation: 1,
            pending_events: Vec::new(),
            pending_renames: Vec::new(),
            known_links: Vec::new(),
        };
        let stable = FileLinkId::from_u128(44);
        remember_rename(&mut checkpoint, 7, stable);
        let renamed = take_rename(&mut checkpoint, 7).expect("rename pair must correlate");
        assert_eq!(renamed, stable);
        remember_known_link(&mut checkpoint, 10, "renamed.md", renamed);
        assert_eq!(known_link_id(&checkpoint, 10, "renamed.md"), Some(stable));

        remember_rename(&mut checkpoint, 7, stable);
        let moved = take_rename(&mut checkpoint, 7).expect("move pair must correlate");
        remember_known_link(&mut checkpoint, 11, "renamed.md", moved);
        assert_eq!(known_link_id(&checkpoint, 11, "renamed.md"), Some(stable));
    }

    #[cfg(windows)]
    #[test]
    fn scoped_scan_stays_inside_selected_root_and_preserves_object_identity() {
        let workspace = tempfile::tempdir().expect("workspace");
        let selected = workspace.path().join("selected");
        let sibling = workspace.path().join("sibling");
        std::fs::create_dir_all(selected.join("nested")).expect("selected tree");
        std::fs::create_dir(&sibling).expect("sibling");
        let before = selected.join("nested").join("before.md");
        std::fs::write(&before, "scoped content").expect("selected file");
        std::fs::write(sibling.join("outside.md"), "must not appear").expect("sibling file");
        let provider = WindowsFilesystemProvider::new();
        let mut first = Vec::new();
        let summary = provider
            .initial_scan_root(&selected, &mut |event| {
                first.push(event);
                Ok(())
            })
            .expect("first scoped scan");
        assert_eq!(
            summary.scan.emitted_events,
            u64::try_from(first.len()).expect("event count")
        );
        assert!(first.iter().any(
            |event| matches!(event, FilesystemEvent::LinkObserved { link } if link.name == "before.md")
        ));
        assert!(!first.iter().any(
            |event| matches!(event, FilesystemEvent::LinkObserved { link } if link.name == "outside.md")
        ));
        let first_object = first.iter().find_map(|event| match event {
            FilesystemEvent::ObjectObserved { object }
                if object.metadata.kind == FileKind::File =>
            {
                Some(object.object_key)
            }
            _ => None,
        });

        let after = selected.join("nested").join("after.md");
        std::fs::rename(before, &after).expect("rename selected file");
        let mut second = Vec::new();
        let second_summary = provider
            .initial_scan_root(&selected, &mut |event| {
                second.push(event);
                Ok(())
            })
            .expect("second scoped scan");
        let second_object = second.iter().find_map(|event| match event {
            FilesystemEvent::ObjectObserved { object }
                if object.metadata.kind == FileKind::File =>
            {
                Some(object.object_key)
            }
            _ => None,
        });
        assert_eq!(summary.volume.volume_id, second_summary.volume.volume_id);
        assert_eq!(first_object, second_object);
        assert!(second.iter().any(
            |event| matches!(event, FilesystemEvent::LinkObserved { link } if link.name == "after.md")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn scoped_scan_observes_excluded_directory_but_skips_its_descendants() {
        let workspace = tempfile::tempdir().expect("workspace");
        let selected = workspace.path().join("selected");
        let generated = selected.join("NoDe_MoDuLeS");
        std::fs::create_dir_all(&generated).expect("generated tree");
        std::fs::write(selected.join("visible.ts"), "visible source").expect("visible file");
        std::fs::write(generated.join("hidden.ts"), "generated source").expect("hidden file");

        let provider = WindowsFilesystemProvider::new();
        let prepared = provider
            .prepare_scan_root(&selected)
            .expect("prepared root");
        let options = ScopedScanOptions::excluding(["node_modules"]);
        let mut events = Vec::new();
        provider
            .scan_prepared_root_with_options(&prepared, &options, &mut |event| {
                events.push(event);
                Ok(())
            })
            .expect("excluded scan");

        assert!(events.iter().any(
            |event| matches!(event, FilesystemEvent::LinkObserved { link } if link.name == "NoDe_MoDuLeS")
        ));
        assert!(events.iter().any(
            |event| matches!(event, FilesystemEvent::LinkObserved { link } if link.name == "visible.ts")
        ));
        assert!(!events.iter().any(
            |event| matches!(event, FilesystemEvent::LinkObserved { link } if link.name == "hidden.ts")
        ));
    }
}
