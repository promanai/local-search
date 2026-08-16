#![forbid(unsafe_code)]

//! Portable `FilesystemProvider` facade over the authenticated `WinFS` broker protocol.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use localsearch_broker_api::{
    BROKER_CODEC_VERSION, BROKER_PROTOCOL_VERSION, BrokerErrorCode, BrokerOperation, BrokerPayload,
    BrokerRequest, BrokerResponse, MAX_BROKER_PAGE_EVENTS, ScanMode, ScanPage,
};
use localsearch_platform_core::{
    ChangeBatch, FilesystemEventSink, FilesystemProvider, PlatformCapabilities, PlatformError,
    PlatformErrorKind, PlatformResult, ProviderCheckpoint, ScanSummary, VolumeDescriptor,
};
use thiserror::Error;

const BROKER_DEADLINE_MS: u32 = 5_000;
const SCAN_DEADLINE: Duration = Duration::from_mins(30);

/// Redacted transport-level failure before a broker response exists.
#[derive(Clone, Copy, Debug, Error)]
pub enum BrokerTransportError {
    /// Broker endpoint is not reachable or closed.
    #[error("WinFS broker is unavailable")]
    Unavailable,
    /// Transport operation exceeded its deadline.
    #[error("WinFS broker transport deadline exceeded")]
    DeadlineExceeded,
    /// Caller cancelled the exchange.
    #[error("WinFS broker transport request cancelled")]
    Cancelled,
    /// Frame or response did not satisfy the versioned contract.
    #[error("WinFS broker returned an incompatible response")]
    Incompatible,
}

/// One-request broker transport port, suitable for real Named Pipes and deterministic tests.
pub trait BrokerTransport: Send + Sync {
    /// Invoke one independently versioned broker request.
    ///
    /// # Errors
    ///
    /// Returns only redacted transport categories.
    fn invoke(&self, request: &BrokerRequest) -> Result<BrokerResponse, BrokerTransportError>;
}

/// Windows current-user client for an explicitly selected elevated broker endpoint.
#[derive(Clone, Debug)]
pub struct NamedPipeBrokerTransport {
    pipe_name: String,
}

impl NamedPipeBrokerTransport {
    /// Create a client bound to one versioned local broker pipe.
    #[must_use]
    pub fn new(pipe_name: String) -> Self {
        Self { pipe_name }
    }
}

impl BrokerTransport for NamedPipeBrokerTransport {
    fn invoke(&self, request: &BrokerRequest) -> Result<BrokerResponse, BrokerTransportError> {
        #[cfg(windows)]
        {
            use localsearch_broker_api::{decode_frame, encode_frame};
            use localsearch_local_transport::windows_pipe::{
                WindowsPipeError, round_trip_frame_cancellable,
            };

            let encoded = encode_frame(request).map_err(|_| BrokerTransportError::Incompatible)?;
            let frame = round_trip_frame_cancellable(
                &self.pipe_name,
                &encoded,
                Duration::from_millis(u64::from(BROKER_DEADLINE_MS) + 1_000),
                &|| false,
            )
            .map_err(|error| match error {
                WindowsPipeError::DeadlineExceeded => BrokerTransportError::DeadlineExceeded,
                WindowsPipeError::Cancelled => BrokerTransportError::Cancelled,
                WindowsPipeError::Frame(_) | WindowsPipeError::Protocol(_) => {
                    BrokerTransportError::Incompatible
                }
                _ => BrokerTransportError::Unavailable,
            })?;
            let response: BrokerResponse =
                decode_frame(&frame).map_err(|_| BrokerTransportError::Incompatible)?;
            response
                .validate()
                .map_err(|_| BrokerTransportError::Incompatible)?;
            Ok(response)
        }
        #[cfg(not(windows))]
        {
            let _ = request;
            Err(BrokerTransportError::Unavailable)
        }
    }
}

/// Broker-backed portable provider. Native journal and handle types remain server-side.
pub struct BrokerFilesystemProvider<T> {
    transport: T,
    capabilities: PlatformCapabilities,
    next_request_id: AtomicU64,
}

impl<T: BrokerTransport> BrokerFilesystemProvider<T> {
    /// Negotiate the broker version and cache portable provider capabilities.
    ///
    /// # Errors
    ///
    /// Returns a portable platform error when negotiation or versions fail.
    pub fn connect(transport: T) -> PlatformResult<Self> {
        let provider = Self {
            transport,
            capabilities: unavailable_capabilities(),
            next_request_id: AtomicU64::new(1),
        };
        let response = provider.invoke(BrokerOperation::BrokerGetCapabilities)?;
        let BrokerPayload::Capabilities(capabilities) = response else {
            return Err(incompatible("capability response type mismatch"));
        };
        if !capabilities
            .protocol_versions
            .contains(&BROKER_PROTOCOL_VERSION)
            || !capabilities.codec_versions.contains(&BROKER_CODEC_VERSION)
        {
            return Err(incompatible("broker version negotiation failed"));
        }
        Ok(Self {
            capabilities: capabilities.provider,
            ..provider
        })
    }

    fn invoke(&self, operation: BrokerOperation) -> PlatformResult<BrokerPayload> {
        let sequence = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let request = BrokerRequest {
            protocol_version: BROKER_PROTOCOL_VERSION,
            codec_version: BROKER_CODEC_VERSION,
            request_id: format!("broker-client-{sequence}"),
            deadline_ms: BROKER_DEADLINE_MS,
            operation,
        };
        let response = self.transport.invoke(&request).map_err(transport_error)?;
        if response.protocol_version != BROKER_PROTOCOL_VERSION
            || response.request_id != request.request_id
        {
            return Err(incompatible("broker response correlation mismatch"));
        }
        match (response.result, response.error) {
            (Some(payload), None) => Ok(payload),
            (None, Some(error)) => Err(wire_error(error.code)),
            _ => Err(incompatible("broker response union mismatch")),
        }
    }

    /// Starts one server-owned paged full-volume scan.
    ///
    /// The caller owns the returned handle and must pull pages or cancel it. This lower-level
    /// surface lets the Agent commit each page without blocking its scheduler for the full scan.
    ///
    /// # Errors
    ///
    /// Returns a portable platform error when the broker cannot create the scan.
    pub fn start_paged_scan(
        &self,
        volume_id: localsearch_core::VolumeId,
        mode: ScanMode,
    ) -> PlatformResult<u64> {
        match self.invoke(BrokerOperation::StartScan { volume_id, mode })? {
            BrokerPayload::ScanStarted { scan_id } => Ok(scan_id),
            _ => Err(incompatible("scan start response mismatch")),
        }
    }

    /// Pulls one bounded page from a server-owned full-volume scan.
    ///
    /// # Errors
    ///
    /// Returns a portable platform error for an expired handle or broker/provider failure.
    pub fn read_paged_scan(&self, scan_id: u64, maximum_events: u16) -> PlatformResult<ScanPage> {
        let maximum_events = maximum_events.min(MAX_BROKER_PAGE_EVENTS);
        if maximum_events == 0 {
            return Err(PlatformError::new(
                PlatformErrorKind::ResourceExhausted,
                "broker_scan",
                "event count must be positive",
            ));
        }
        match self.invoke(BrokerOperation::ReadScanPage {
            scan_id,
            maximum_events,
        })? {
            BrokerPayload::ScanPage(page) => Ok(page),
            _ => Err(incompatible("scan page response mismatch")),
        }
    }

    /// Cancels and releases one server-owned full-volume scan.
    ///
    /// # Errors
    ///
    /// Returns a portable platform error when the broker cannot acknowledge cancellation.
    pub fn cancel_paged_scan(&self, scan_id: u64) -> PlatformResult<()> {
        match self.invoke(BrokerOperation::CancelScan { scan_id })? {
            BrokerPayload::ScanCancelled { scan_id: cancelled } if cancelled == scan_id => Ok(()),
            _ => Err(incompatible("scan cancellation response mismatch")),
        }
    }

    fn scan(
        &self,
        volume: &VolumeDescriptor,
        mode: localsearch_broker_api::ScanMode,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        let scan_id = self.start_paged_scan(volume.volume_id, mode)?;
        let started = Instant::now();
        let mut emitted_events = 0_u64;
        loop {
            if started.elapsed() >= SCAN_DEADLINE {
                let _ = self.cancel_paged_scan(scan_id);
                return Err(PlatformError::new(
                    PlatformErrorKind::ResourceExhausted,
                    "broker_scan",
                    "broker scan exceeded client lifecycle deadline",
                ));
            }
            let page = self.read_paged_scan(scan_id, MAX_BROKER_PAGE_EVENTS)?;
            let event_count = u64::try_from(page.events.len()).map_err(|_| {
                PlatformError::new(
                    PlatformErrorKind::ResourceExhausted,
                    "broker_scan",
                    "event count overflow",
                )
            })?;
            emitted_events = emitted_events.checked_add(event_count).ok_or_else(|| {
                PlatformError::new(
                    PlatformErrorKind::ResourceExhausted,
                    "broker_scan",
                    "event count overflow",
                )
            })?;
            for event in page.events {
                if let Err(error) = sink.emit(event) {
                    let _ = self.cancel_paged_scan(scan_id);
                    return Err(error);
                }
            }
            if page.complete {
                let checkpoint = page
                    .checkpoint
                    .ok_or_else(|| incompatible("completed scan omitted checkpoint"))?;
                return Ok(ScanSummary {
                    checkpoint,
                    emitted_events,
                });
            }
            if event_count == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

impl<T: BrokerTransport> FilesystemProvider for BrokerFilesystemProvider<T> {
    fn capabilities(&self) -> PlatformCapabilities {
        self.capabilities
    }

    fn discover_volumes(&self) -> PlatformResult<Vec<VolumeDescriptor>> {
        match self.invoke(BrokerOperation::DiscoverVolumes)? {
            BrokerPayload::Volumes(volumes) => Ok(volumes),
            _ => Err(incompatible("volume response mismatch")),
        }
    }

    fn initial_scan(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        self.scan(volume, localsearch_broker_api::ScanMode::Initial, sink)
    }

    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        maximum_events: u32,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ChangeBatch> {
        let bounded = maximum_events.min(u32::from(MAX_BROKER_PAGE_EVENTS));
        let maximum_events = u16::try_from(bounded).map_err(|_| {
            PlatformError::new(
                PlatformErrorKind::ResourceExhausted,
                "broker_changes",
                "event count conversion failed",
            )
        })?;
        if maximum_events == 0 {
            return Err(PlatformError::new(
                PlatformErrorKind::ResourceExhausted,
                "broker_changes",
                "event count must be positive",
            ));
        }
        let payload = self.invoke(BrokerOperation::ReadChanges {
            checkpoint: checkpoint.clone(),
            maximum_events,
        })?;
        let BrokerPayload::Changes { events, batch } = payload else {
            return Err(incompatible("change response mismatch"));
        };
        if events.len() != usize::try_from(batch.emitted_events).unwrap_or(usize::MAX)
            || events.len() > usize::from(maximum_events)
        {
            return Err(incompatible("change response count mismatch"));
        }
        for event in events {
            sink.emit(event)?;
        }
        Ok(batch)
    }

    fn reconcile(
        &self,
        volume: &VolumeDescriptor,
        sink: &mut dyn FilesystemEventSink,
    ) -> PlatformResult<ScanSummary> {
        self.scan(volume, localsearch_broker_api::ScanMode::Reconcile, sink)
    }
}

fn unavailable_capabilities() -> PlatformCapabilities {
    use localsearch_platform_core::{
        ChangeTrackingMode, InitialScanMode, PlatformFamily, PrivilegeModel,
    };
    PlatformCapabilities {
        platform: PlatformFamily::Windows,
        initial_scan: InitialScanMode::FilesystemCrawl,
        change_tracking: ChangeTrackingMode::SnapshotOnly,
        privilege_model: PrivilegeModel::OptionalBroker,
        stable_object_ids: false,
        hard_links: false,
        persistent_history: false,
    }
}

fn transport_error(error: BrokerTransportError) -> PlatformError {
    let kind = match error {
        BrokerTransportError::Unavailable => PlatformErrorKind::Unavailable,
        BrokerTransportError::DeadlineExceeded => PlatformErrorKind::ResourceExhausted,
        BrokerTransportError::Cancelled => PlatformErrorKind::Cancelled,
        BrokerTransportError::Incompatible => PlatformErrorKind::Internal,
    };
    PlatformError::new(kind, "broker_transport", "broker transport failed")
}

fn wire_error(code: BrokerErrorCode) -> PlatformError {
    let kind = match code {
        BrokerErrorCode::PermissionDenied => PlatformErrorKind::PermissionDenied,
        BrokerErrorCode::Unsupported => PlatformErrorKind::Unsupported,
        BrokerErrorCode::Unavailable | BrokerErrorCode::NotFound => PlatformErrorKind::Unavailable,
        BrokerErrorCode::SourceHistoryGap => PlatformErrorKind::SourceHistoryGap,
        BrokerErrorCode::ResourceExhausted
        | BrokerErrorCode::DeadlineExceeded
        | BrokerErrorCode::ReplayRejected => PlatformErrorKind::ResourceExhausted,
        BrokerErrorCode::Cancelled => PlatformErrorKind::Cancelled,
        BrokerErrorCode::InvalidRequest
        | BrokerErrorCode::UnsupportedProtocolVersion
        | BrokerErrorCode::UnsupportedCodecVersion
        | BrokerErrorCode::Internal => PlatformErrorKind::Internal,
    };
    PlatformError::new(
        kind,
        "broker_response",
        "broker rejected metadata operation",
    )
}

fn incompatible(detail: &'static str) -> PlatformError {
    PlatformError::new(PlatformErrorKind::Internal, "broker_contract", detail)
}
