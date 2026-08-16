//! Same-logon Windows Named Pipe transport for versioned `LocalSearch` protocols.

use std::{
    ffi::c_void,
    io,
    mem::size_of,
    ptr, slice, thread,
    time::{Duration, Instant},
};

#[cfg(feature = "agent-wire")]
use localsearch_agent_api::{AgentRequest, AgentResponse, decode_frame, encode_frame};
use thiserror::Error;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_LISTENING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_GROUPS,
        TOKEN_QUERY, TokenGroups,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, ImpersonateNamedPipeClient,
            PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            PeekNamedPipe, SetNamedPipeHandleState, WaitNamedPipeW,
        },
        SystemServices::SE_GROUP_LOGON_ID,
        Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
    },
};

const PIPE_BUFFER_BYTES: u32 = 65_536;
const PIPE_CLIENT_ACCESS_MASK: &str = "0x12019b";
const RESPONSE_ACK: u8 = 0xAC;
const MAX_TRANSPORT_FRAME_BYTES: usize = 1_048_576;

/// Failure of the protocol-neutral bounded frame envelope.
#[derive(Clone, Copy, Debug, Error)]
pub enum TransportFrameError {
    /// Declared payload is larger than one MiB.
    #[error("local transport frame exceeds maximum size")]
    TooLarge,
    /// Length prefix and payload are absent or inconsistent.
    #[error("local transport frame is incomplete")]
    Incomplete,
}

/// Windows IPC adapter failure with no request/query/path data in its display form.
#[derive(Debug, Error)]
pub enum WindowsPipeError {
    /// Native endpoint/security/IO operation failed.
    #[error("Windows Named Pipe operation `{operation}` failed: {source}")]
    Io {
        /// Stable diagnostic operation name.
        operation: &'static str,
        /// Native status.
        #[source]
        source: io::Error,
    },
    /// Opaque frame did not satisfy the bounded transport envelope.
    #[error("local transport frame rejected: {0}")]
    Frame(#[from] TransportFrameError),
    /// Connected client is not in the Agent's logon session.
    #[error("Named Pipe client identity did not match the Agent logon SID")]
    Unauthorized,
    /// Endpoint operation exceeded its transport deadline.
    #[error("Named Pipe transport deadline exceeded")]
    DeadlineExceeded,
    /// The caller cancelled the in-flight exchange.
    #[error("Named Pipe transport request cancelled")]
    Cancelled,
    /// Pipe endpoint name is not a valid versioned `LocalSearch` endpoint.
    #[error("invalid local LocalSearch pipe name")]
    InvalidEndpoint,
    /// An authenticated upper-layer protocol handler could not produce a valid frame.
    #[error("local protocol handler failed: {0}")]
    Protocol(&'static str),
}

/// Result type for the Windows IPC adapter.
pub type WindowsPipeResult<T> = Result<T, WindowsPipeError>;

/// Returns the v0.1 endpoint isolated by the current logon SID.
///
/// # Errors
///
/// Returns a native security error when the current logon SID cannot be resolved.
pub fn default_pipe_name() -> WindowsPipeResult<String> {
    Ok(format!(
        r"\\.\pipe\LocalSearch\Agent\v1\{}",
        current_logon_sid()?
    ))
}

/// One first-instance, same-logon, local-only server endpoint.
pub struct NamedPipeServer {
    handle: OwnedHandle,
    logon_sid: String,
}

impl NamedPipeServer {
    /// Creates the first pipe instance with an explicit logon-SID DACL.
    ///
    /// # Errors
    ///
    /// Fails closed for an invalid/squatted name or any descriptor/native error.
    pub fn bind(pipe_name: &str) -> WindowsPipeResult<Self> {
        Self::bind_authorized_logon_sid(pipe_name, &current_logon_sid()?)
    }

    /// Creates the first pipe instance for one explicitly authorized logon SID.
    ///
    /// Elevated brokers use this constructor because their process SID is not the client SID.
    /// The DACL and post-connect impersonation check both use the same supplied identity.
    ///
    /// # Errors
    ///
    /// Fails closed for an invalid endpoint/SID, squatted name, descriptor, or native error.
    pub fn bind_authorized_logon_sid(
        pipe_name: &str,
        authorized_logon_sid: &str,
    ) -> WindowsPipeResult<Self> {
        validate_pipe_name(pipe_name)?;
        if authorized_logon_sid.is_empty()
            || authorized_logon_sid.len() > 184
            || authorized_logon_sid.contains(['\0', ')', '('])
        {
            return Err(WindowsPipeError::InvalidEndpoint);
        }
        let logon_sid = authorized_logon_sid.to_owned();
        let descriptor = SecurityDescriptor::for_logon_sid(&logon_sid)?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                .map_err(|_| WindowsPipeError::InvalidEndpoint)?,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let wide = wide_null(pipe_name);
        // SAFETY: the endpoint and security descriptor are live, NUL-terminated/initialized, and
        // the returned owned handle is checked before use. No native type crosses this module.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                2_000,
                &raw mut attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_io("create_named_pipe"));
        }
        Ok(Self {
            handle: OwnedHandle(handle),
            logon_sid,
        })
    }

    /// Serves exactly one authenticated request and disconnects the client.
    ///
    /// # Errors
    ///
    /// Returns a bounded transport/authentication/codec error. The service is never dispatched
    /// before the client logon SID is verified.
    #[cfg(feature = "agent-wire")]
    pub fn serve_one<F>(&self, handler: F, transport_deadline: Duration) -> WindowsPipeResult<()>
    where
        F: FnOnce(AgentRequest, &dyn Fn() -> bool) -> AgentResponse,
    {
        self.serve_frame(
            |frame, disconnected| {
                let request: AgentRequest = decode_frame(&frame)
                    .map_err(|_| WindowsPipeError::Protocol("Agent request frame rejected"))?;
                let response = handler(request, disconnected);
                encode_frame(&response)
                    .map_err(|_| WindowsPipeError::Protocol("Agent response encoding failed"))
            },
            transport_deadline,
        )
    }

    /// Serves one authenticated, bounded opaque protocol frame and disconnects the client.
    ///
    /// The frame retains its four-byte length prefix. Protocol crates remain responsible for
    /// typed decoding, validation, and response encoding after transport authentication.
    ///
    /// # Errors
    ///
    /// Returns a bounded transport/authentication error or a handler-provided contract error.
    pub fn serve_frame<F>(&self, handler: F, transport_deadline: Duration) -> WindowsPipeResult<()>
    where
        F: FnOnce(Vec<u8>, &dyn Fn() -> bool) -> WindowsPipeResult<Vec<u8>>,
    {
        self.serve_frame_cancellable(handler, transport_deadline, &|| false)
    }

    /// Serves one authenticated frame while observing a process/service shutdown signal.
    ///
    /// # Errors
    ///
    /// Returns [`WindowsPipeError::Cancelled`] on shutdown, plus the same bounded errors as
    /// [`Self::serve_frame`].
    pub fn serve_frame_cancellable<F>(
        &self,
        handler: F,
        transport_deadline: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> WindowsPipeResult<()>
    where
        F: FnOnce(Vec<u8>, &dyn Fn() -> bool) -> WindowsPipeResult<Vec<u8>>,
    {
        let started = Instant::now();
        wait_for_client(self.handle.0, started, transport_deadline, cancelled)?;
        let result = (|| {
            let frame = read_frame(self.handle.0, started, transport_deadline, cancelled)?;
            verify_client_logon_sid(self.handle.0, &self.logon_sid)?;
            let disconnected = || cancelled() || pipe_disconnected(self.handle.0);
            let encoded = handler(frame, &disconnected)?;
            validate_encoded_frame(&encoded)?;
            write_all(
                self.handle.0,
                &encoded,
                started,
                transport_deadline,
                cancelled,
            )?;
            let mut acknowledgement = [0_u8; 1];
            read_exact(
                self.handle.0,
                &mut acknowledgement,
                started,
                transport_deadline,
                cancelled,
            )?;
            if acknowledgement[0] != RESPONSE_ACK {
                return Err(TransportFrameError::Incomplete.into());
            }
            Ok(())
        })();
        // SAFETY: this is the connected server handle owned by `self`; disconnect is idempotent
        // for cleanup and does not transfer ownership.
        unsafe {
            DisconnectNamedPipe(self.handle.0);
        }
        result
    }
}

/// Sends one bounded request and receives its matching response.
///
/// # Errors
///
/// Returns a local endpoint, timeout, authentication/DACL, IO, or wire-contract error.
#[cfg(feature = "agent-wire")]
pub fn round_trip(
    pipe_name: &str,
    request: &AgentRequest,
    transport_deadline: Duration,
) -> WindowsPipeResult<AgentResponse> {
    round_trip_cancellable(pipe_name, request, transport_deadline, &|| false)
}

/// Sends one bounded request and permits cooperative cancellation while waiting for the Agent.
///
/// Dropping the client handle after cancellation lets the Agent observe a disconnected caller and
/// stop work between cancellable query phases.
///
/// # Errors
///
/// Returns [`WindowsPipeError::Cancelled`] when `cancelled` becomes true, or the same bounded
/// endpoint/contract errors as [`round_trip`].
#[cfg(feature = "agent-wire")]
pub fn round_trip_cancellable(
    pipe_name: &str,
    request: &AgentRequest,
    transport_deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<AgentResponse> {
    let encoded = encode_frame(request)
        .map_err(|_| WindowsPipeError::Protocol("Agent request encoding failed"))?;
    let frame = round_trip_frame_cancellable(pipe_name, &encoded, transport_deadline, cancelled)?;
    let response: AgentResponse = decode_frame(&frame)
        .map_err(|_| WindowsPipeError::Protocol("Agent response frame rejected"))?;
    response
        .validate()
        .map_err(|_| WindowsPipeError::Protocol("Agent response contract rejected"))?;
    Ok(response)
}

/// Exchanges one bounded opaque protocol frame with cancellation.
///
/// # Errors
///
/// Returns a local endpoint, timeout, cancellation, authentication/DACL, IO, or frame-bound error.
pub fn round_trip_frame_cancellable(
    pipe_name: &str,
    encoded: &[u8],
    transport_deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<Vec<u8>> {
    validate_pipe_name(pipe_name)?;
    validate_encoded_frame(encoded)?;
    let started = Instant::now();
    let handle = connect_client(pipe_name, started, transport_deadline, cancelled)?;
    write_all(handle.0, encoded, started, transport_deadline, cancelled)?;
    let frame = read_frame(handle.0, started, transport_deadline, cancelled)?;
    write_all(
        handle.0,
        &[RESPONSE_ACK],
        started,
        transport_deadline,
        cancelled,
    )?;
    Ok(frame)
}

fn validate_pipe_name(pipe_name: &str) -> WindowsPipeResult<()> {
    if !(pipe_name.starts_with(r"\\.\pipe\LocalSearch\Agent\v1\")
        || pipe_name.starts_with(r"\\.\pipe\LocalSearch\WinFS\v1\"))
        || pipe_name.len() > 256
        || pipe_name.contains('\0')
    {
        return Err(WindowsPipeError::InvalidEndpoint);
    }
    Ok(())
}

fn connect_client(
    pipe_name: &str,
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<OwnedHandle> {
    let wide = wide_null(pipe_name);
    loop {
        check_transport(started, deadline, cancelled)?;
        // SAFETY: the name is a live NUL-terminated buffer and all optional pointers/handles are
        // null as documented. SQOS limits impersonation to identity inspection.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            let handle = OwnedHandle(handle);
            let mut mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
            // SAFETY: `handle` is a valid connected named-pipe client; mode is initialized.
            if unsafe {
                SetNamedPipeHandleState(handle.0, &raw mut mode, ptr::null_mut(), ptr::null_mut())
            } == 0
            {
                return Err(last_io("set_client_pipe_mode"));
            }
            return Ok(handle);
        }
        // SAFETY: reads thread-local last error immediately after CreateFileW.
        let error = unsafe { GetLastError() };
        if error != ERROR_PIPE_BUSY && error != ERROR_FILE_NOT_FOUND {
            return Err(WindowsPipeError::Io {
                operation: "connect_pipe",
                source: io::Error::from_raw_os_error(i32::try_from(error).unwrap_or(i32::MAX)),
            });
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        let wait_ms = u32::try_from(remaining.as_millis().min(10)).unwrap_or(10);
        // SAFETY: the pipe name remains live and NUL-terminated for this call.
        unsafe {
            WaitNamedPipeW(wide.as_ptr(), wait_ms.max(1));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_client(
    handle: HANDLE,
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<()> {
    loop {
        check_transport(started, deadline, cancelled)?;
        // SAFETY: `handle` is an owned server pipe in nonblocking mode; no OVERLAPPED is used.
        if unsafe { ConnectNamedPipe(handle, ptr::null_mut()) } != 0 {
            return Ok(());
        }
        // SAFETY: reads thread-local last error immediately after ConnectNamedPipe.
        let error = unsafe { GetLastError() };
        if error == ERROR_PIPE_CONNECTED {
            return Ok(());
        }
        if error != ERROR_PIPE_LISTENING && error != ERROR_NO_DATA {
            return Err(WindowsPipeError::Io {
                operation: "accept_pipe_client",
                source: io::Error::from_raw_os_error(i32::try_from(error).unwrap_or(i32::MAX)),
            });
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn read_frame(
    handle: HANDLE,
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    read_exact(handle, &mut prefix, started, deadline, cancelled)?;
    let length =
        usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| TransportFrameError::TooLarge)?;
    if length > MAX_TRANSPORT_FRAME_BYTES {
        return Err(TransportFrameError::TooLarge.into());
    }
    let mut frame = Vec::with_capacity(length + 4);
    frame.extend_from_slice(&prefix);
    frame.resize(length + 4, 0);
    read_exact(handle, &mut frame[4..], started, deadline, cancelled)?;
    Ok(frame)
}

fn read_exact(
    handle: HANDLE,
    mut buffer: &mut [u8],
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<()> {
    while !buffer.is_empty() {
        check_transport(started, deadline, cancelled)?;
        let chunk = buffer.len().min(u32::MAX as usize);
        let mut read = 0_u32;
        // SAFETY: buffer is writable for `chunk`, count storage is initialized, and the handle is
        // a connected named-pipe endpoint in nonblocking byte mode.
        let success = unsafe {
            ReadFile(
                handle,
                buffer.as_mut_ptr(),
                u32::try_from(chunk).unwrap_or(u32::MAX),
                &raw mut read,
                ptr::null_mut(),
            )
        };
        if success == 0 {
            // SAFETY: reads thread-local last error immediately after ReadFile.
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_DATA {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            return Err(WindowsPipeError::Io {
                operation: "read_pipe",
                source: io::Error::from_raw_os_error(i32::try_from(error).unwrap_or(i32::MAX)),
            });
        }
        let count = usize::try_from(read).unwrap_or(0);
        if count == 0 {
            thread::sleep(Duration::from_millis(1));
        } else {
            buffer = &mut buffer[count..];
        }
    }
    Ok(())
}

fn write_all(
    handle: HANDLE,
    mut buffer: &[u8],
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<()> {
    while !buffer.is_empty() {
        check_transport(started, deadline, cancelled)?;
        let chunk = buffer.len().min(u32::MAX as usize);
        let mut written = 0_u32;
        // SAFETY: buffer is readable for `chunk`, count storage is initialized, and the handle is
        // a connected named-pipe endpoint in nonblocking byte mode.
        let success = unsafe {
            WriteFile(
                handle,
                buffer.as_ptr(),
                u32::try_from(chunk).unwrap_or(u32::MAX),
                &raw mut written,
                ptr::null_mut(),
            )
        };
        if success == 0 {
            // SAFETY: reads thread-local last error immediately after WriteFile.
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_DATA {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            return Err(WindowsPipeError::Io {
                operation: "write_pipe",
                source: io::Error::from_raw_os_error(i32::try_from(error).unwrap_or(i32::MAX)),
            });
        }
        let count = usize::try_from(written).unwrap_or(0);
        if count == 0 {
            thread::sleep(Duration::from_millis(1));
        } else {
            buffer = &buffer[count..];
        }
    }
    Ok(())
}

fn verify_client_logon_sid(handle: HANDLE, expected: &str) -> WindowsPipeResult<()> {
    // SAFETY: handle is a connected server pipe and this is called only after the request bytes
    // have been read. The return value is checked and all paths revert impersonation.
    if unsafe { ImpersonateNamedPipeClient(handle) } == 0 {
        return Err(last_io("impersonate_pipe_client"));
    }
    let actual = current_thread_logon_sid();
    // SAFETY: balances successful impersonation before any result is evaluated.
    let reverted = unsafe { RevertToSelf() };
    if reverted == 0 {
        return Err(last_io("revert_pipe_impersonation"));
    }
    if actual? == expected {
        Ok(())
    } else {
        Err(WindowsPipeError::Unauthorized)
    }
}

fn pipe_disconnected(handle: HANDLE) -> bool {
    let mut available = 0_u32;
    // SAFETY: handle is the connected server endpoint; all optional buffers are null and the only
    // requested output is a valid byte-count pointer.
    unsafe {
        PeekNamedPipe(
            handle,
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &raw mut available,
            ptr::null_mut(),
        ) == 0
    }
}

/// Returns the current process token's logon SID in canonical SDDL form.
///
/// # Errors
///
/// Returns a native token/formatting error when the identity cannot be resolved.
pub fn current_logon_sid() -> WindowsPipeResult<String> {
    let mut token = ptr::null_mut();
    // SAFETY: output points to initialized handle storage; pseudo process handle is always valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(last_io("open_process_token"));
    }
    logon_sid_from_token(&OwnedHandle(token))
}

fn current_thread_logon_sid() -> WindowsPipeResult<String> {
    let mut token = ptr::null_mut();
    // SAFETY: called while impersonating; output points to initialized handle storage.
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) } == 0 {
        return Err(last_io("open_client_thread_token"));
    }
    logon_sid_from_token(&OwnedHandle(token))
}

fn logon_sid_from_token(token: &OwnedHandle) -> WindowsPipeResult<String> {
    let mut required = 0_u32;
    // SAFETY: first call intentionally provides no buffer to retrieve required size.
    unsafe {
        GetTokenInformation(token.0, TokenGroups, ptr::null_mut(), 0, &raw mut required);
    }
    if required == 0 {
        return Err(last_io("size_token_groups"));
    }
    let required_size = usize::try_from(required).map_err(|_| WindowsPipeError::Io {
        operation: "size_token_groups",
        source: io::Error::other("token group buffer exceeds address space"),
    })?;
    let mut words = vec![0_usize; required_size.div_ceil(size_of::<usize>())];
    // SAFETY: buffer has the exact required writable size and output length storage is valid.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenGroups,
            words.as_mut_ptr().cast(),
            required,
            &raw mut required,
        )
    } == 0
    {
        return Err(last_io("read_token_groups"));
    }
    // SAFETY: successful GetTokenInformation(TokenGroups) initialized TOKEN_GROUPS followed by
    // GroupCount SID_AND_ATTRIBUTES entries within the aligned storage.
    let groups = unsafe { &*words.as_ptr().cast::<TOKEN_GROUPS>() };
    let count = usize::try_from(groups.GroupCount).map_err(|_| WindowsPipeError::Unauthorized)?;
    // SAFETY: Windows guarantees this trailing-array layout for a successful TokenGroups query.
    let entries = unsafe { slice::from_raw_parts(groups.Groups.as_ptr(), count) };
    let logon_flag = u32::try_from(SE_GROUP_LOGON_ID).unwrap_or(0xC000_0000);
    let sid = entries
        .iter()
        .find(|entry| entry.Attributes & logon_flag == logon_flag)
        .map(|entry| entry.Sid)
        .ok_or(WindowsPipeError::Unauthorized)?;
    sid_to_string(sid)
}

fn sid_to_string(sid: *mut c_void) -> WindowsPipeResult<String> {
    let mut output = ptr::null_mut();
    // SAFETY: SID comes from a successful token query and output pointer storage is valid.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut output) } == 0 {
        return Err(last_io("format_logon_sid"));
    }
    let owned = LocalAllocation(output.cast());
    let mut length = 0_usize;
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated LocalAlloc UTF-16 string.
    unsafe {
        while *output.add(length) != 0 {
            length += 1;
        }
    }
    // SAFETY: the measured range is within the returned NUL-terminated allocation.
    let wide = unsafe { slice::from_raw_parts(output, length) };
    let value = String::from_utf16(wide).map_err(|error| WindowsPipeError::Io {
        operation: "format_logon_sid",
        source: io::Error::new(io::ErrorKind::InvalidData, error),
    })?;
    drop(owned);
    Ok(value)
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn for_logon_sid(logon_sid: &str) -> WindowsPipeResult<Self> {
        let sddl = format!("D:P(A;;{PIPE_CLIENT_ACCESS_MASK};;;{logon_sid})");
        let wide = wide_null(&sddl);
        let mut descriptor = ptr::null_mut();
        // SAFETY: SDDL is NUL-terminated and output storage is valid; ownership is transferred to
        // `SecurityDescriptor` and released with LocalFree.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_io("create_pipe_security_descriptor"));
        }
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: pointer was allocated by ConvertStringSecurityDescriptor... and is owned here.
        unsafe {
            LocalFree(self.0.cast());
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: handle was returned by an owning Win32 open/create call and is closed once here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: pointer was allocated by a LocalAlloc-returning Win32 API and is owned here.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn check_transport(
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> WindowsPipeResult<()> {
    if cancelled() {
        Err(WindowsPipeError::Cancelled)
    } else if started.elapsed() >= deadline {
        Err(WindowsPipeError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn validate_encoded_frame(frame: &[u8]) -> WindowsPipeResult<()> {
    let prefix: [u8; 4] = frame
        .get(..4)
        .ok_or(TransportFrameError::Incomplete)?
        .try_into()
        .map_err(|_| TransportFrameError::Incomplete)?;
    let length =
        usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| TransportFrameError::TooLarge)?;
    if length > MAX_TRANSPORT_FRAME_BYTES {
        return Err(TransportFrameError::TooLarge.into());
    }
    if frame.len().checked_sub(4) != Some(length) {
        return Err(TransportFrameError::Incomplete.into());
    }
    Ok(())
}

fn last_io(operation: &'static str) -> WindowsPipeError {
    WindowsPipeError::Io {
        operation,
        source: io::Error::last_os_error(),
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
