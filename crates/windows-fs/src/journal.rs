use std::path::{Path, PathBuf};

use localsearch_platform_core::{PlatformError, PlatformErrorKind, PlatformResult};

use crate::record::SanitizedUsnRecord;

const READ_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JournalState {
    pub identity: u64,
    pub first_position: i64,
    pub next_position: i64,
    pub lowest_valid_position: i64,
}

#[derive(Debug)]
pub(crate) struct JournalPage {
    pub next_position: i64,
    pub records: Vec<SanitizedUsnRecord>,
}

pub(crate) struct JournalSession {
    native: native::Session,
}

impl JournalSession {
    pub(crate) fn open(root: &Path) -> PlatformResult<Self> {
        native::Session::open(root).map(|native| Self { native })
    }

    pub(crate) fn query(&self) -> PlatformResult<JournalState> {
        self.native.query()
    }

    pub(crate) fn read(&self, start: i64, identity: u64) -> PlatformResult<JournalPage> {
        self.native.read(start, identity)
    }

    pub(crate) fn resolve_child(
        &self,
        parent_reference: u64,
        name: &str,
    ) -> PlatformResult<PathBuf> {
        self.native.resolve_child(parent_reference, name)
    }
}

#[cfg(windows)]
mod native {
    #![allow(
        unsafe_code,
        reason = "audited leaf FFI owns volume/file handles and copies bounded kernel outputs"
    )]

    use std::{
        ffi::OsString,
        mem::size_of,
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::{Path, PathBuf},
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0,
            FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FileIdType, GetFinalPathNameByHandleW, OPEN_EXISTING, OpenFileById,
            VOLUME_NAME_DOS,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V1,
                USN_JOURNAL_DATA_V0,
            },
        },
    };

    use super::{
        JournalPage, JournalState, Path as PublicPath, PlatformError, PlatformErrorKind,
        PlatformResult, READ_BUFFER_BYTES,
    };
    use crate::record::decode_page;

    pub(super) struct Session {
        handle: OwnedHandle,
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: constructed only from one valid owned handle and closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Session {
        pub(super) fn open(root: &PublicPath) -> PlatformResult<Self> {
            let device = volume_device_path(root)?;
            let wide = device.encode_wide().chain(Some(0)).collect::<Vec<_>>();
            // SAFETY: the device path is NUL terminated, optional pointers are null, and ownership is checked.
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(native_error("open_usn_volume"));
            }
            Ok(Self {
                handle: OwnedHandle(raw),
            })
        }

        pub(super) fn query(&self) -> PlatformResult<JournalState> {
            let mut state = USN_JOURNAL_DATA_V0::default();
            let mut returned = 0_u32;
            // SAFETY: the handle is live and the output points to aligned writable storage of the supplied size.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle.0,
                    FSCTL_QUERY_USN_JOURNAL,
                    std::ptr::null(),
                    0,
                    (&raw mut state).cast(),
                    checked_size::<USN_JOURNAL_DATA_V0>()?,
                    &raw mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(native_error("query_usn_journal"));
            }
            if usize::try_from(returned).unwrap_or(0) < size_of::<USN_JOURNAL_DATA_V0>() {
                return Err(PlatformError::new(
                    PlatformErrorKind::Io,
                    "query_usn_journal",
                    "kernel returned a truncated journal state",
                ));
            }
            Ok(JournalState {
                identity: state.UsnJournalID,
                first_position: state.FirstUsn,
                next_position: state.NextUsn,
                lowest_valid_position: state.LowestValidUsn,
            })
        }

        pub(super) fn read(&self, start: i64, identity: u64) -> PlatformResult<JournalPage> {
            let input = READ_USN_JOURNAL_DATA_V1 {
                StartUsn: start,
                ReasonMask: u32::MAX,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                BytesToWaitFor: 0,
                UsnJournalID: identity,
                MinMajorVersion: 2,
                MaxMajorVersion: 2,
            };
            let mut output = vec![0_u8; READ_BUFFER_BYTES];
            let mut returned = 0_u32;
            // SAFETY: input and output point to live storage for the exact supplied lengths; the call is synchronous.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle.0,
                    FSCTL_READ_USN_JOURNAL,
                    (&raw const input).cast(),
                    checked_size::<READ_USN_JOURNAL_DATA_V1>()?,
                    output.as_mut_ptr().cast(),
                    checked_len(output.len())?,
                    &raw mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(native_error("read_usn_journal"));
            }
            let returned = usize::try_from(returned).map_err(|_| {
                PlatformError::new(
                    PlatformErrorKind::Io,
                    "read_usn_journal",
                    "returned byte count overflow",
                )
            })?;
            if returned < size_of::<i64>() || returned > output.len() {
                return Err(PlatformError::new(
                    PlatformErrorKind::Io,
                    "read_usn_journal",
                    "kernel returned an invalid journal page length",
                ));
            }
            output.truncate(returned);
            let (next_position, records) = decode_page(&output)?;
            Ok(JournalPage {
                next_position,
                records,
            })
        }

        pub(super) fn resolve_child(
            &self,
            parent_reference: u64,
            name: &str,
        ) -> PlatformResult<PathBuf> {
            let descriptor = FILE_ID_DESCRIPTOR {
                dwSize: checked_size::<FILE_ID_DESCRIPTOR>()?,
                Type: FileIdType,
                Anonymous: FILE_ID_DESCRIPTOR_0 {
                    FileId: i64::from_ne_bytes(parent_reference.to_ne_bytes()),
                },
            };
            // SAFETY: the descriptor is fully initialized for FileIdType and the returned owned handle is checked.
            let raw = unsafe {
                OpenFileById(
                    self.handle.0,
                    &raw const descriptor,
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    FILE_FLAG_BACKUP_SEMANTICS,
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(resolve_error());
            }
            let parent = OwnedHandle(raw);
            let mut buffer = vec![0_u16; 32_768];
            // SAFETY: the handle is live and buffer is writable for the supplied UTF-16 capacity.
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    parent.0,
                    buffer.as_mut_ptr(),
                    checked_len(buffer.len())?,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            };
            if length == 0 {
                return Err(resolve_error());
            }
            let length = usize::try_from(length).map_err(|_| {
                PlatformError::new(
                    PlatformErrorKind::Io,
                    "resolve_usn_parent",
                    "resolved path length overflow",
                )
            })?;
            if length >= buffer.len() {
                return Err(PlatformError::new(
                    PlatformErrorKind::ResourceExhausted,
                    "resolve_usn_parent",
                    "resolved path exceeded the bounded UTF-16 buffer",
                ));
            }
            buffer.truncate(length);
            Ok(PathBuf::from(OsString::from_wide(&buffer)).join(name))
        }
    }

    fn checked_size<T>() -> PlatformResult<u32> {
        checked_len(size_of::<T>())
    }

    fn checked_len(length: usize) -> PlatformResult<u32> {
        u32::try_from(length).map_err(|_| {
            PlatformError::new(
                PlatformErrorKind::ResourceExhausted,
                "windows_buffer_size",
                "native buffer length exceeds u32",
            )
        })
    }

    fn volume_device_path(root: &Path) -> PlatformResult<OsString> {
        let units = root.as_os_str().encode_wide().collect::<Vec<_>>();
        if units.len() >= 2 && units[1] == u16::from(b':') {
            let letter = char::from_u32(u32::from(units[0])).ok_or_else(|| {
                PlatformError::new(
                    PlatformErrorKind::Unsupported,
                    "open_usn_volume",
                    "drive letter is not valid Unicode",
                )
            })?;
            return Ok(OsString::from(format!(r"\\.\{letter}:")));
        }
        let display = root.as_os_str().to_string_lossy();
        if display.starts_with(r"\\?\Volume{") {
            return Ok(OsString::from(display.trim_end_matches(['\\', '/'])));
        }
        Err(PlatformError::new(
            PlatformErrorKind::Unsupported,
            "open_usn_volume",
            "volume has no supported Windows device path",
        ))
    }

    fn native_error(operation: &'static str) -> PlatformError {
        let error = std::io::Error::last_os_error();
        PlatformError::new(
            native_error_kind(error.raw_os_error()),
            operation,
            error.to_string(),
        )
    }

    fn resolve_error() -> PlatformError {
        let error = std::io::Error::last_os_error();
        let kind = resolve_error_kind(error.raw_os_error());
        PlatformError::new(kind, "resolve_usn_parent", error.to_string())
    }

    const fn resolve_error_kind(code: Option<i32>) -> PlatformErrorKind {
        match code {
            Some(87) => {
                // USN records may outlive their namespace parent during rapid subtree deletion.
                // ERROR_INVALID_PARAMETER from OpenFileById is transient for this lookup; later
                // canonical delete records still have to advance the checkpoint.
                PlatformErrorKind::Unavailable
            }
            _ => native_error_kind(code),
        }
    }

    const fn native_error_kind(code: Option<i32>) -> PlatformErrorKind {
        match code {
            Some(5) => PlatformErrorKind::PermissionDenied,
            Some(2 | 3 | 21 | 1178) => PlatformErrorKind::Unavailable,
            Some(1179) => PlatformErrorKind::Unsupported,
            Some(1181) => PlatformErrorKind::SourceHistoryGap,
            _ => PlatformErrorKind::Io,
        }
    }

    const _: () = assert!(READ_BUFFER_BYTES >= 8);

    #[cfg(test)]
    mod tests {
        use super::{PlatformErrorKind, resolve_error_kind};

        #[test]
        fn vanished_usn_parent_is_transient_but_unrelated_native_errors_remain_fatal() {
            assert_eq!(resolve_error_kind(Some(87)), PlatformErrorKind::Unavailable);
            assert_eq!(resolve_error_kind(Some(6)), PlatformErrorKind::Io);
        }
    }
}

#[cfg(not(windows))]
mod native {
    use super::{
        JournalPage, JournalState, Path, PathBuf, PlatformError, PlatformErrorKind, PlatformResult,
    };

    pub(super) struct Session;

    impl Session {
        pub(super) fn open(_root: &Path) -> PlatformResult<Self> {
            Err(unsupported())
        }
        pub(super) fn query(&self) -> PlatformResult<JournalState> {
            Err(unsupported())
        }
        pub(super) fn read(&self, _start: i64, _identity: u64) -> PlatformResult<JournalPage> {
            Err(unsupported())
        }
        pub(super) fn resolve_child(
            &self,
            _parent_reference: u64,
            _name: &str,
        ) -> PlatformResult<PathBuf> {
            Err(unsupported())
        }
    }

    fn unsupported() -> PlatformError {
        PlatformError::new(
            PlatformErrorKind::Unsupported,
            "usn_journal",
            "Windows USN journal requires Windows",
        )
    }
}
