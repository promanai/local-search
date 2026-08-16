use std::{
    collections::{HashMap, VecDeque},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Instant,
};

use localsearch_core::{DocumentId, FileKind};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State, WebviewWindow, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_opener::OpenerExt;

use crate::{
    DesktopAgentClient, DesktopClientError, DesktopContentSearchResult, DesktopErrorCode,
    DesktopSearchResult, NamedPipeAgentTransport,
};

const DEFAULT_HOTKEY: &str = "Ctrl+Space";
const MAX_HOTKEY_BYTES: usize = 64;
const ACTIVATION_SAMPLE_CAPACITY: usize = 512;
const UX_QUERY_ARGUMENT: &str = "--localsearch-ux-query";

struct DesktopRuntime {
    client: Arc<DesktopAgentClient<NamedPipeAgentTransport>>,
    hotkey: String,
    activations: ActivationMetrics,
}

#[derive(Default)]
struct ActivationMetrics {
    next_token: std::sync::atomic::AtomicU64,
    pending: Mutex<HashMap<u64, Instant>>,
    samples_micros: Mutex<VecDeque<u64>>,
}

impl ActivationMetrics {
    fn begin(&self) -> Result<u64, DesktopClientError> {
        let token = self
            .next_token
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        self.pending
            .lock()
            .map_err(|_| internal_error())?
            .insert(token, Instant::now());
        Ok(token)
    }

    fn discard(&self, token: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&token);
        }
    }

    fn acknowledge(&self, token: u64) -> Result<Option<u64>, DesktopClientError> {
        let Some(started) = self
            .pending
            .lock()
            .map_err(|_| internal_error())?
            .remove(&token)
        else {
            return Ok(None);
        };
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let mut samples = self.samples_micros.lock().map_err(|_| internal_error())?;
        if samples.len() == ACTIVATION_SAMPLE_CAPACITY {
            samples.pop_front();
        }
        samples.push_back(micros);
        Ok(Some(micros))
    }

    fn summary(&self) -> Result<ActivationSummary, DesktopClientError> {
        let mut samples = self
            .samples_micros
            .lock()
            .map_err(|_| internal_error())?
            .iter()
            .copied()
            .collect::<Vec<_>>();
        samples.sort_unstable();
        Ok(ActivationSummary {
            samples: u32::try_from(samples.len()).unwrap_or(u32::MAX),
            p50_micros: percentile(&samples, 50),
            p95_micros: percentile(&samples, 95),
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct ActivationEvent {
    token: u64,
}

#[derive(Clone, Debug, Serialize)]
struct UxQueryEvent {
    query: &'static str,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct ActivationSummary {
    samples: u32,
    p50_micros: Option<u64>,
    p95_micros: Option<u64>,
}

#[derive(Debug, Serialize)]
struct DesktopBootstrap {
    hotkey: String,
    service_available: bool,
    content_search_available: bool,
    ux_evidence_enabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the evidence record preserves independent DOM invariants for machine-readable audit"
)]
struct UxLayoutSnapshot {
    reason: String,
    viewport_width: u32,
    viewport_height: u32,
    device_pixel_ratio: f64,
    input_focused: bool,
    launcher_fits_viewport: bool,
    document_horizontal_overflow: bool,
    results_horizontal_overflow: bool,
    results_scroll_available: bool,
    selected_result_visible: bool,
    content_overflow_exercised: bool,
    content_overflow_managed: bool,
    result_count: u32,
    pass: bool,
}

impl UxLayoutSnapshot {
    fn validate(&self) -> Result<(), DesktopClientError> {
        let bounded = !self.reason.is_empty()
            && self.reason.len() <= 32
            && self.viewport_width > 0
            && self.viewport_width <= 10_000
            && self.viewport_height > 0
            && self.viewport_height <= 10_000
            && self.device_pixel_ratio.is_finite()
            && (0.5..=8.0).contains(&self.device_pixel_ratio)
            && self.result_count <= 50
            && self.pass == self.accepted();
        if bounded {
            Ok(())
        } else {
            Err(DesktopClientError::new(
                DesktopErrorCode::InvalidRequest,
                "UX evidence sample is invalid",
            ))
        }
    }

    fn accepted(&self) -> bool {
        self.input_focused
            && self.launcher_fits_viewport
            && !self.document_horizontal_overflow
            && !self.results_horizontal_overflow
            && self.results_scroll_available
            && self.selected_result_visible
            && self.content_overflow_managed
            && self.result_count <= 50
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DesktopItemAction {
    Open,
    OpenFolder,
    CopyPath,
}

#[derive(Debug, Serialize)]
struct DesktopActionResult {
    resolved_path: String,
}

#[derive(Debug, Serialize)]
struct DesktopSearchEvidence {
    elapsed_micros: u64,
    backend_micros: Option<u64>,
    result_count: u32,
    error: Option<DesktopErrorCode>,
}

#[tauri::command(rename_all = "snake_case")]
async fn desktop_search(
    state: State<'_, DesktopRuntime>,
    request_id: String,
    query: String,
) -> Result<DesktopSearchResult, DesktopClientError> {
    let started = Instant::now();
    let client = Arc::clone(&state.client);
    let result = tauri::async_runtime::spawn_blocking(move || client.search(request_id, query))
        .await
        .map_err(|_| internal_error())?;
    if std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_some() {
        let evidence = match &result {
            Ok(search) => DesktopSearchEvidence {
                elapsed_micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                backend_micros: Some(search.response.took_micros),
                result_count: u32::try_from(search.response.hits.len()).unwrap_or(u32::MAX),
                error: None,
            },
            Err(error) => DesktopSearchEvidence {
                elapsed_micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                backend_micros: None,
                result_count: 0,
                error: Some(error.code),
            },
        };
        let encoded = serde_json::to_string(&evidence).map_err(|_| internal_error())?;
        eprintln!("START010_SEARCH_JSON={encoded}");
    }
    result
}

#[tauri::command(rename_all = "snake_case")]
async fn desktop_content_search(
    state: State<'_, DesktopRuntime>,
    request_id: String,
    query: String,
) -> Result<DesktopContentSearchResult, DesktopClientError> {
    let client = Arc::clone(&state.client);
    tauri::async_runtime::spawn_blocking(move || client.search_content(request_id, query))
        .await
        .map_err(|_| internal_error())?
}

#[tauri::command(rename_all = "snake_case")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command extractors and owned IPC arguments are passed by value"
)]
fn desktop_cancel(
    state: State<'_, DesktopRuntime>,
    request_id: String,
) -> Result<bool, DesktopClientError> {
    state.client.cancel(&request_id)
}

#[tauri::command(rename_all = "snake_case")]
async fn desktop_health(
    state: State<'_, DesktopRuntime>,
    request_id: String,
) -> Result<bool, DesktopClientError> {
    let client = Arc::clone(&state.client);
    tauri::async_runtime::spawn_blocking(move || client.health(&request_id))
        .await
        .map_err(|_| internal_error())?
}

#[tauri::command(rename_all = "snake_case")]
async fn desktop_ready(
    state: State<'_, DesktopRuntime>,
) -> Result<DesktopBootstrap, DesktopClientError> {
    let request_id = "desktop-ready".to_owned();
    let client = Arc::clone(&state.client);
    let (service_available, content_search_available) =
        tauri::async_runtime::spawn_blocking(move || {
            let available = client.health(&request_id).unwrap_or(false);
            let content = available && client.content_search_available("desktop-ready-content");
            (available, content)
        })
        .await
        .map_err(|_| internal_error())?;
    Ok(DesktopBootstrap {
        hotkey: state.hotkey.clone(),
        service_available,
        content_search_available,
        ux_evidence_enabled: std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_some(),
    })
}

#[tauri::command(rename_all = "snake_case")]
async fn desktop_content_available(
    state: State<'_, DesktopRuntime>,
) -> Result<bool, DesktopClientError> {
    let client = Arc::clone(&state.client);
    tauri::async_runtime::spawn_blocking(move || {
        client.content_search_available("desktop-content-available")
    })
    .await
    .map_err(|_| internal_error())
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri deserializes command payloads into owned extractors"
)]
fn desktop_record_ux_snapshot(snapshot: UxLayoutSnapshot) -> Result<bool, DesktopClientError> {
    if std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_none() {
        return Ok(false);
    }
    snapshot.validate()?;
    let encoded = serde_json::to_string(&snapshot).map_err(|_| internal_error())?;
    eprintln!("START010_LAYOUT_JSON={encoded}");
    Ok(true)
}

#[tauri::command]
fn desktop_record_ui_search_result(accepted: bool) -> bool {
    if std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_none() {
        return false;
    }
    eprintln!("START010_UI_SEARCH_ACCEPTED={}", u8::from(accepted));
    true
}

#[tauri::command]
fn desktop_record_ui_stall(stall_millis: u32) -> Result<bool, DesktopClientError> {
    if std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_none() {
        return Ok(false);
    }
    if !(100..=60_000).contains(&stall_millis) {
        return Err(DesktopClientError::new(
            DesktopErrorCode::InvalidRequest,
            "UX stall evidence is outside the accepted range",
        ));
    }
    eprintln!("START010_UI_STALL_MILLIS={stall_millis}");
    Ok(true)
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri injects the target WebView window as an owned command extractor"
)]
fn desktop_hide(window: WebviewWindow) -> Result<(), DesktopClientError> {
    window.hide().map_err(|_| internal_error())
}

#[tauri::command(rename_all = "snake_case")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command state is an owned extractor"
)]
fn desktop_ack_focus(
    state: State<'_, DesktopRuntime>,
    token: u64,
) -> Result<Option<u64>, DesktopClientError> {
    let sample = state.activations.acknowledge(token)?;
    if std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_some()
        && let Some(micros) = sample
    {
        eprintln!("START010_FOCUS_MICROS={micros}");
    }
    Ok(sample)
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command state is an owned extractor"
)]
fn desktop_activation_metrics(
    state: State<'_, DesktopRuntime>,
) -> Result<ActivationSummary, DesktopClientError> {
    state.activations.summary()
}

#[tauri::command(rename_all = "snake_case")]
async fn desktop_item_action(
    app: tauri::AppHandle,
    state: State<'_, DesktopRuntime>,
    request_id: String,
    document_id: String,
    action: DesktopItemAction,
) -> Result<DesktopActionResult, DesktopClientError> {
    let document_id = DocumentId::from_str(&document_id).map_err(|_| {
        DesktopClientError::new(
            DesktopErrorCode::InvalidRequest,
            "Selected item identity is invalid",
        )
    })?;
    let client = Arc::clone(&state.client);
    let item = tauri::async_runtime::spawn_blocking(move || {
        client.resolve_action_target(&request_id, document_id)
    })
    .await
    .map_err(|_| internal_error())??;
    match action {
        DesktopItemAction::Open => app
            .opener()
            .open_path(item.resolved_path.clone(), None::<&str>)
            .map_err(|_| item_unavailable())?,
        DesktopItemAction::OpenFolder => {
            if item.kind == FileKind::Directory {
                app.opener()
                    .open_path(item.resolved_path.clone(), None::<&str>)
                    .map_err(|_| item_unavailable())?;
            } else {
                app.opener()
                    .reveal_item_in_dir(&item.resolved_path)
                    .map_err(|_| item_unavailable())?;
            }
        }
        DesktopItemAction::CopyPath => app
            .clipboard()
            .write_text(item.resolved_path.clone())
            .map_err(|_| internal_error())?,
    }
    Ok(DesktopActionResult {
        resolved_path: item.resolved_path,
    })
}

fn activate_window(app: &tauri::AppHandle) -> Result<(), DesktopClientError> {
    let state = app.state::<DesktopRuntime>();
    let token = state.activations.begin()?;
    let result = (|| {
        let window = app.get_webview_window("main").ok_or_else(internal_error)?;
        window.show().map_err(|_| internal_error())?;
        window.unminimize().map_err(|_| internal_error())?;
        window.set_focus().map_err(|_| internal_error())?;
        app.emit_to("main", "desktop://focus-search", ActivationEvent { token })
            .map_err(|_| internal_error())
    })();
    if result.is_err() {
        state.activations.discard(token);
    }
    result
}

fn configure_close_to_hide(window: &WebviewWindow) {
    let window = window.clone();
    window.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = window.hide();
        }
    });
}

fn configured_hotkey() -> Result<String, DesktopClientError> {
    let hotkey = std::env::var("LOCALSEARCH_HOTKEY").unwrap_or_else(|_| DEFAULT_HOTKEY.to_owned());
    if hotkey.trim().is_empty() || hotkey.len() > MAX_HOTKEY_BYTES {
        return Err(DesktopClientError::new(
            DesktopErrorCode::InvalidRequest,
            "Configured global shortcut is invalid",
        ));
    }
    Ok(hotkey)
}

fn configured_agent_transport() -> Result<NamedPipeAgentTransport, DesktopClientError> {
    let Some(pipe) = std::env::var_os("LOCALSEARCH_AGENT_PIPE") else {
        return Ok(NamedPipeAgentTransport::default());
    };
    let pipe = pipe.to_string_lossy().into_owned();
    if pipe.len() > 256
        || !pipe.starts_with(r"\\.\pipe\LocalSearch\Agent\v1\")
        || pipe.contains('\0')
    {
        return Err(DesktopClientError::new(
            DesktopErrorCode::InvalidRequest,
            "Configured Agent endpoint is invalid",
        ));
    }
    Ok(NamedPipeAgentTransport::with_pipe_name(pipe))
}

fn controlled_ux_query(arguments: &[String], evidence_enabled: bool) -> Option<&'static str> {
    if !evidence_enabled {
        return None;
    }
    let mut values = arguments
        .windows(2)
        .filter(|pair| pair[0] == UX_QUERY_ARGUMENT)
        .map(|pair| pair[1].as_str());
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    match value {
        "architecture" => Some("architecture"),
        "churn" => Some("churn"),
        _ => None,
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let index = (sorted.len().saturating_mul(percentile).saturating_add(99) / 100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted.get(index).copied()
}

fn item_unavailable() -> DesktopClientError {
    DesktopClientError::new(
        DesktopErrorCode::ItemUnavailable,
        "The selected item is offline, missing, or no longer accessible",
    )
}

fn internal_error() -> DesktopClientError {
    DesktopClientError::new(
        DesktopErrorCode::Internal,
        "Desktop operation could not be completed",
    )
}

/// Runs the Windows resident Tauri application.
///
/// # Errors
///
/// Returns startup, plugin, global-shortcut registration, or event-loop errors.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let hotkey = configured_hotkey()?;
    let runtime = DesktopRuntime {
        client: Arc::new(DesktopAgentClient::new(configured_agent_transport()?)),
        hotkey: hotkey.clone(),
        activations: ActivationMetrics::default(),
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, arguments, _cwd| {
                let _ = activate_window(app);
                if let Some(query) = controlled_ux_query(
                    &arguments,
                    std::env::var_os("LOCALSEARCH_UX_EVIDENCE").is_some(),
                ) {
                    let _ = app.emit_to(
                        "main",
                        "desktop://controlled-ux-query",
                        UxQueryEvent { query },
                    );
                }
            },
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let _ = activate_window(app);
                    }
                })
                .build(),
        )
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(runtime)
        .setup(move |app| {
            app.global_shortcut().register(hotkey.as_str())?;
            let window = app
                .get_webview_window("main")
                .ok_or("main WebView window was not created")?;
            configure_close_to_hide(&window);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_search,
            desktop_content_search,
            desktop_cancel,
            desktop_health,
            desktop_ready,
            desktop_content_available,
            desktop_hide,
            desktop_ack_focus,
            desktop_activation_metrics,
            desktop_record_ux_snapshot,
            desktop_record_ui_search_result,
            desktop_record_ui_stall,
            desktop_item_action,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ActivationMetrics, DesktopSearchEvidence, UxLayoutSnapshot, controlled_ux_query, percentile,
    };
    use crate::DesktopErrorCode;
    use std::time::Duration;

    #[test]
    fn percentile_uses_nearest_rank_without_inventing_samples() {
        assert_eq!(percentile(&[], 95), None);
        assert_eq!(percentile(&[10], 95), Some(10));
        assert_eq!(percentile(&[10, 20, 30, 40], 50), Some(20));
        assert_eq!(percentile(&[10, 20, 30, 40], 95), Some(40));
    }

    #[test]
    fn activation_ack_is_exact_once_and_bounded() {
        let metrics = ActivationMetrics::default();
        let token = metrics.begin().expect("metric must begin");
        std::thread::sleep(Duration::from_millis(1));
        assert!(
            metrics
                .acknowledge(token)
                .expect("ack must succeed")
                .is_some()
        );
        assert_eq!(
            metrics.acknowledge(token).expect("second ack is safe"),
            None
        );
        let summary = metrics.summary().expect("summary must succeed");
        assert_eq!(summary.samples, 1);
        assert!(summary.p50_micros.is_some());
    }

    #[test]
    fn ux_layout_evidence_is_bounded_and_cannot_claim_a_false_pass() {
        let mut snapshot = UxLayoutSnapshot {
            reason: "results".to_owned(),
            viewport_width: 760,
            viewport_height: 540,
            device_pixel_ratio: 2.0,
            input_focused: true,
            launcher_fits_viewport: true,
            document_horizontal_overflow: false,
            results_horizontal_overflow: false,
            results_scroll_available: true,
            selected_result_visible: true,
            content_overflow_exercised: true,
            content_overflow_managed: true,
            result_count: 50,
            pass: true,
        };
        assert!(snapshot.validate().is_ok());
        snapshot.results_horizontal_overflow = true;
        assert!(snapshot.validate().is_err());
        snapshot.pass = false;
        assert!(snapshot.validate().is_ok());
        snapshot.result_count = 51;
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn search_evidence_contains_metrics_and_redacted_error_only() {
        let encoded = serde_json::to_string(&DesktopSearchEvidence {
            elapsed_micros: 42,
            backend_micros: None,
            result_count: 0,
            error: Some(DesktopErrorCode::Cancelled),
        })
        .expect("evidence JSON");
        assert!(encoded.contains("cancelled"));
        assert!(!encoded.contains("request_id"));
        assert!(!encoded.contains("query"));
        assert!(!encoded.contains("resolved_path"));
    }

    #[test]
    fn controlled_ux_query_is_evidence_only_bounded_and_allowlisted() {
        let arguments = vec![
            "localsearch-desktop.exe".to_owned(),
            "--localsearch-ux-query".to_owned(),
            "architecture".to_owned(),
        ];
        assert_eq!(controlled_ux_query(&arguments, true), Some("architecture"));
        assert_eq!(controlled_ux_query(&arguments, false), None);

        let mut hostile = arguments.clone();
        hostile[2] = "C:\\private\\document.txt".to_owned();
        assert_eq!(controlled_ux_query(&hostile, true), None);

        let mut duplicate = arguments.clone();
        duplicate.extend(["--localsearch-ux-query".to_owned(), "churn".to_owned()]);
        assert_eq!(controlled_ux_query(&duplicate, true), None);
    }
}
