#![forbid(unsafe_code)]

//! Bounded metadata-only `WinFS` broker service over portable provider contracts.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use localsearch_broker_api::{
    BROKER_CODEC_VERSION, BROKER_PROTOCOL_VERSION, BrokerCapabilities, BrokerContractError,
    BrokerErrorCode, BrokerOperation, BrokerOperationName, BrokerPayload, BrokerRequest,
    BrokerResponse, MAX_BROKER_FRAME_BYTES, MAX_BROKER_PAGE_EVENTS, ScanMode, ScanPage,
};
use localsearch_core::FilesystemEvent;
use localsearch_platform_core::{
    FilesystemEventSink, FilesystemProvider, PlatformError, PlatformErrorKind, PlatformResult,
    ProviderCheckpoint, ScanSummary,
};

const MAX_ACTIVE_SCANS: usize = 4;
const SCAN_QUEUE_EVENTS: usize = 256;
const REPLAY_WINDOW_REQUESTS: usize = 4_096;

/// Stateful broker dispatcher. The provider remains the only owner of native filesystem details.
pub struct BrokerService<P> {
    provider: Arc<P>,
    replay: Mutex<ReplayWindow>,
    scans: Mutex<HashMap<u64, ScanState>>,
    next_scan_id: AtomicU64,
}

impl<P> BrokerService<P>
where
    P: FilesystemProvider + 'static,
{
    /// Create a service around an explicitly selected provider mode.
    #[must_use]
    pub fn new(provider: Arc<P>) -> Self {
        Self {
            provider,
            replay: Mutex::new(ReplayWindow::new(REPLAY_WINDOW_REQUESTS)),
            scans: Mutex::new(HashMap::new()),
            next_scan_id: AtomicU64::new(1),
        }
    }

    /// Validate, replay-protect, and dispatch one authenticated request.
    #[must_use]
    pub fn dispatch(&self, request: BrokerRequest, cancelled: &dyn Fn() -> bool) -> BrokerResponse {
        let request_id = if request.request_id.len() <= 64 {
            request.request_id.clone()
        } else {
            String::new()
        };
        if let Err(error) = request.validate() {
            return contract_failure(request_id, &error);
        }
        let replay_result = self
            .replay
            .lock()
            .map_err(|_| ())
            .and_then(|mut replay| replay.admit(&request.request_id));
        if replay_result.is_err() {
            return BrokerResponse::failure(
                request_id,
                BrokerErrorCode::ReplayRejected,
                "request identifier was replayed or replay state is unavailable",
            );
        }
        let started = Instant::now();
        let deadline = Duration::from_millis(u64::from(request.effective_deadline_ms()));
        if cancelled() {
            return BrokerResponse::failure(
                request_id,
                BrokerErrorCode::Cancelled,
                "broker request cancelled",
            );
        }
        let result = self.dispatch_operation(request.operation, started, deadline, cancelled);
        match result {
            Ok(payload) => BrokerResponse::success(request_id, payload),
            Err(error) => BrokerResponse::failure(request_id, error.code, error.message),
        }
    }

    fn dispatch_operation(
        &self,
        operation: BrokerOperation,
        started: Instant,
        deadline: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<BrokerPayload, ServiceError> {
        check_request(started, deadline, cancelled)?;
        match operation {
            BrokerOperation::BrokerGetCapabilities => {
                Ok(BrokerPayload::Capabilities(self.capabilities()))
            }
            BrokerOperation::DiscoverVolumes => self
                .provider
                .discover_volumes()
                .map(BrokerPayload::Volumes)
                .map_err(|error| platform_error(&error)),
            BrokerOperation::StartScan { volume_id, mode } => {
                self.start_scan(volume_id, mode, started, deadline, cancelled)
            }
            BrokerOperation::ReadScanPage {
                scan_id,
                maximum_events,
            } => self.read_scan_page(scan_id, maximum_events),
            BrokerOperation::CancelScan { scan_id } => self.cancel_scan(scan_id),
            BrokerOperation::ReadChanges {
                checkpoint,
                maximum_events,
            } => self.read_changes(&checkpoint, maximum_events),
        }
    }

    fn capabilities(&self) -> BrokerCapabilities {
        BrokerCapabilities {
            protocol_versions: vec![BROKER_PROTOCOL_VERSION],
            codec_versions: vec![BROKER_CODEC_VERSION],
            allowed_operations: vec![
                BrokerOperationName::BrokerGetCapabilities,
                BrokerOperationName::DiscoverVolumes,
                BrokerOperationName::StartScan,
                BrokerOperationName::ReadScanPage,
                BrokerOperationName::CancelScan,
                BrokerOperationName::ReadChanges,
            ],
            maximum_frame_bytes: u32::try_from(MAX_BROKER_FRAME_BYTES).unwrap_or(u32::MAX),
            maximum_page_events: MAX_BROKER_PAGE_EVENTS,
            maximum_active_scans: u16::try_from(MAX_ACTIVE_SCANS).unwrap_or(u16::MAX),
            provider: self.provider.capabilities(),
        }
    }

    fn start_scan(
        &self,
        volume_id: localsearch_core::VolumeId,
        mode: ScanMode,
        started: Instant,
        deadline: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<BrokerPayload, ServiceError> {
        let volume = self
            .provider
            .discover_volumes()
            .map_err(|error| platform_error(&error))?
            .into_iter()
            .find(|volume| volume.volume_id == volume_id)
            .ok_or_else(|| ServiceError::new(BrokerErrorCode::NotFound, "volume is unavailable"))?;
        check_request(started, deadline, cancelled)?;
        let mut scans = self.scans.lock().map_err(|_| internal())?;
        if scans.len() >= MAX_ACTIVE_SCANS {
            return Err(ServiceError::new(
                BrokerErrorCode::ResourceExhausted,
                "active scan limit reached",
            ));
        }
        let scan_id = self.next_scan_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = sync_channel(SCAN_QUEUE_EVENTS);
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancellation);
        let provider = Arc::clone(&self.provider);
        let worker = thread::spawn(move || {
            let mut sink = QueueSink {
                sender: sender.clone(),
                cancellation: Arc::clone(&worker_cancel),
            };
            let result = match mode {
                ScanMode::Initial => provider.initial_scan(&volume, &mut sink),
                ScanMode::Reconcile => provider.reconcile(&volume, &mut sink),
            };
            send_terminal(&sender, &worker_cancel, result);
        });
        scans.insert(
            scan_id,
            ScanState {
                receiver,
                cancellation,
                worker: Some(worker),
                pending_terminal: None,
            },
        );
        Ok(BrokerPayload::ScanStarted { scan_id })
    }

    fn read_scan_page(
        &self,
        scan_id: u64,
        maximum_events: u16,
    ) -> Result<BrokerPayload, ServiceError> {
        let mut scans = self.scans.lock().map_err(|_| internal())?;
        let state = scans
            .get_mut(&scan_id)
            .ok_or_else(|| ServiceError::new(BrokerErrorCode::NotFound, "scan is not active"))?;
        let mut events = Vec::with_capacity(usize::from(maximum_events));
        let mut terminal = state.pending_terminal.take();
        while events.len() < usize::from(maximum_events) && terminal.is_none() {
            match state.receiver.try_recv() {
                Ok(ScanMessage::Event(event)) => events.push(event),
                Ok(ScanMessage::Terminal(result)) => terminal = Some(result),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    terminal = Some(Err(PlatformError::new(
                        PlatformErrorKind::Internal,
                        "scan_worker",
                        "scan producer stopped without terminal state",
                    )));
                }
            }
        }
        if !events.is_empty() && terminal.is_some() {
            state.pending_terminal = terminal;
            return Ok(BrokerPayload::ScanPage(ScanPage {
                events,
                checkpoint: None,
                complete: false,
                has_more: true,
            }));
        }
        if let Some(result) = terminal {
            let mut state = scans.remove(&scan_id).ok_or_else(internal)?;
            join_worker(&mut state);
            return match result {
                Ok(summary) => Ok(BrokerPayload::ScanPage(ScanPage {
                    events,
                    checkpoint: Some(summary.checkpoint),
                    complete: true,
                    has_more: false,
                })),
                Err(error) => Err(platform_error(&error)),
            };
        }
        Ok(BrokerPayload::ScanPage(ScanPage {
            events,
            checkpoint: None,
            complete: false,
            has_more: true,
        }))
    }

    fn cancel_scan(&self, scan_id: u64) -> Result<BrokerPayload, ServiceError> {
        let mut state = self
            .scans
            .lock()
            .map_err(|_| internal())?
            .remove(&scan_id)
            .ok_or_else(|| ServiceError::new(BrokerErrorCode::NotFound, "scan is not active"))?;
        state.cancellation.store(true, Ordering::Release);
        join_worker(&mut state);
        Ok(BrokerPayload::ScanCancelled { scan_id })
    }

    fn read_changes(
        &self,
        checkpoint: &ProviderCheckpoint,
        maximum_events: u16,
    ) -> Result<BrokerPayload, ServiceError> {
        let mut events = Vec::with_capacity(usize::from(maximum_events));
        let batch = self
            .provider
            .read_changes(checkpoint, u32::from(maximum_events), &mut |event| {
                events.push(event);
                Ok(())
            })
            .map_err(|error| platform_error(&error))?;
        Ok(BrokerPayload::Changes { events, batch })
    }
}

impl<P> Drop for BrokerService<P> {
    fn drop(&mut self) {
        if let Ok(mut scans) = self.scans.lock() {
            for state in scans.values() {
                state.cancellation.store(true, Ordering::Release);
            }
            for state in scans.values_mut() {
                join_worker(state);
            }
            scans.clear();
        }
    }
}

struct ReplayWindow {
    capacity: usize,
    order: VecDeque<String>,
    present: HashSet<String>,
}

impl ReplayWindow {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::new(),
            present: HashSet::new(),
        }
    }

    fn admit(&mut self, request_id: &str) -> Result<(), ()> {
        if !self.present.insert(request_id.to_owned()) {
            return Err(());
        }
        self.order.push_back(request_id.to_owned());
        if self.order.len() > self.capacity
            && let Some(expired) = self.order.pop_front()
        {
            self.present.remove(&expired);
        }
        Ok(())
    }
}

struct ScanState {
    receiver: Receiver<ScanMessage>,
    cancellation: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    pending_terminal: Option<PlatformResult<ScanSummary>>,
}

enum ScanMessage {
    Event(FilesystemEvent),
    Terminal(PlatformResult<ScanSummary>),
}

struct QueueSink {
    sender: SyncSender<ScanMessage>,
    cancellation: Arc<AtomicBool>,
}

impl FilesystemEventSink for QueueSink {
    fn emit(&mut self, event: FilesystemEvent) -> PlatformResult<()> {
        send_with_backpressure(&self.sender, &self.cancellation, ScanMessage::Event(event))
    }
}

fn send_with_backpressure(
    sender: &SyncSender<ScanMessage>,
    cancellation: &AtomicBool,
    mut message: ScanMessage,
) -> PlatformResult<()> {
    loop {
        if cancellation.load(Ordering::Acquire) {
            return Err(PlatformError::new(
                PlatformErrorKind::Cancelled,
                "scan_queue",
                "scan was cancelled",
            ));
        }
        match sender.try_send(message) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                message = returned;
                thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(PlatformError::new(
                    PlatformErrorKind::Cancelled,
                    "scan_queue",
                    "scan consumer disconnected",
                ));
            }
        }
    }
}

fn send_terminal(
    sender: &SyncSender<ScanMessage>,
    cancellation: &AtomicBool,
    result: PlatformResult<ScanSummary>,
) {
    let _ = send_with_backpressure(sender, cancellation, ScanMessage::Terminal(result));
}

fn join_worker(state: &mut ScanState) {
    if let Some(worker) = state.worker.take() {
        let _ = worker.join();
    }
}

fn check_request(
    started: Instant,
    deadline: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ServiceError> {
    if cancelled() {
        Err(ServiceError::new(
            BrokerErrorCode::Cancelled,
            "broker request cancelled",
        ))
    } else if started.elapsed() >= deadline {
        Err(ServiceError::new(
            BrokerErrorCode::DeadlineExceeded,
            "broker request deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

struct ServiceError {
    code: BrokerErrorCode,
    message: &'static str,
}

impl ServiceError {
    const fn new(code: BrokerErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }
}

fn platform_error(error: &PlatformError) -> ServiceError {
    let code = match error.kind {
        PlatformErrorKind::PermissionDenied => BrokerErrorCode::PermissionDenied,
        PlatformErrorKind::Unsupported | PlatformErrorKind::InvalidCheckpoint => {
            BrokerErrorCode::Unsupported
        }
        PlatformErrorKind::Unavailable | PlatformErrorKind::Io => BrokerErrorCode::Unavailable,
        PlatformErrorKind::SourceHistoryGap => BrokerErrorCode::SourceHistoryGap,
        PlatformErrorKind::ResourceExhausted => BrokerErrorCode::ResourceExhausted,
        PlatformErrorKind::Cancelled => BrokerErrorCode::Cancelled,
        PlatformErrorKind::Internal => BrokerErrorCode::Internal,
    };
    ServiceError::new(code, "filesystem metadata operation failed")
}

fn contract_failure(request_id: String, error: &BrokerContractError) -> BrokerResponse {
    let code = match error {
        BrokerContractError::UnsupportedProtocolVersion => {
            BrokerErrorCode::UnsupportedProtocolVersion
        }
        BrokerContractError::UnsupportedCodecVersion => BrokerErrorCode::UnsupportedCodecVersion,
        _ => BrokerErrorCode::InvalidRequest,
    };
    BrokerResponse::failure(request_id, code, "broker request contract rejected")
}

const fn internal() -> ServiceError {
    ServiceError::new(BrokerErrorCode::Internal, "broker state is unavailable")
}
