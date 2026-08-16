import {
  SearchSession,
  idleStatus,
  kindLabel,
  matchLabel,
  parentPath,
  resultStatus,
  searchModePresentation,
} from "./state.mjs";
import { collectLayoutSnapshot } from "./ux-evidence.mjs";

const invoke = window.__TAURI__?.core?.invoke;
const listen = window.__TAURI__?.event?.listen;
const input = document.querySelector("#search-input");
const results = document.querySelector("#results");
const emptyState = document.querySelector("#empty-state");
const status = document.querySelector("#status");
const statusRow = document.querySelector(".status-row");
const count = document.querySelector("#result-count");
const hotkeyLabel = document.querySelector("#hotkey-label");
const hideButton = document.querySelector("#hide-button");
const searchMode = document.querySelector("#search-mode");
const nameModeButton = document.querySelector("#name-mode");
const contentModeButton = document.querySelector("#content-mode");
const session = new SearchSession();
let debounceTimer = null;
let serviceAvailable = false;
let reconnectTimer = null;
let auxiliaryRequestSequence = 0;
let uxEvidenceEnabled = false;
let stallMonitor = null;
let contentMode = false;

function startUiStallMonitor() {
  if (!uxEvidenceEnabled || stallMonitor !== null) return;
  const intervalMillis = 50;
  let previous = window.performance.now();
  stallMonitor = window.setInterval(() => {
    const current = window.performance.now();
    const excess = current - previous - intervalMillis;
    previous = current;
    if (excess >= 100) {
      const bounded = Math.min(60000, Math.round(excess));
      void invoke("desktop_record_ui_stall", { stall_millis: bounded }).catch(() => {});
    }
  }, intervalMillis);
}

function recordUxSnapshot(reason) {
  if (!uxEvidenceEnabled) return;
  window.requestAnimationFrame(() => {
    const snapshot = collectLayoutSnapshot(document, window, reason);
    void invoke("desktop_record_ux_snapshot", { snapshot }).catch(() => {});
  });
}

function setStatus(message, isError = false) {
  status.textContent = message;
  statusRow.classList.toggle("error", isError);
}

function safeErrorCode(error) {
  if (error && typeof error === "object" && typeof error.code === "string") return error.code;
  return "internal";
}

function requestId(prefix) {
  auxiliaryRequestSequence += 1;
  return `${prefix}-${Date.now().toString(36)}-${auxiliaryRequestSequence.toString(36)}`;
}

function scheduleReconnect() {
  if (reconnectTimer !== null) return;
  reconnectTimer = window.setInterval(async () => {
    try {
      serviceAvailable = await invoke("desktop_health", { request_id: requestId("health") });
      if (serviceAvailable) {
        const contentAvailable = await invoke("desktop_content_available").catch(() => false);
        searchMode.hidden = !contentAvailable;
        window.clearInterval(reconnectTimer);
        reconnectTimer = null;
        setStatus(input.value.trim() ? "Search service reconnected" : "Type to search your catalog");
        if (input.value.trim()) scheduleSearch();
      }
    } catch {
      serviceAvailable = false;
    }
  }, 2000);
}

function renderResults() {
  results.replaceChildren();
  session.hits.forEach((hit, index) => {
    const option = document.createElement("div");
    option.className = "result";
    option.id = `result-${index}`;
    option.setAttribute("role", "option");
    option.setAttribute("aria-selected", String(index === session.selectedIndex));
    option.tabIndex = -1;

    const main = document.createElement("div");
    main.className = "result-main";
    const name = document.createElement("div");
    name.className = "result-name";
    name.textContent = hit.name;
    const path = document.createElement("div");
    path.className = "result-path";
    path.textContent = parentPath(hit.resolved_path);
    path.title = hit.resolved_path;
    main.append(name, path);

    const meta = document.createElement("div");
    meta.className = "result-meta";
    const type = document.createElement("span");
    type.className = "badge";
    type.textContent = kindLabel(hit.kind, hit.extension);
    const match = document.createElement("span");
    match.className = "badge";
    match.textContent = matchLabel(hit.match_type);
    meta.append(type, match);

    const actions = document.createElement("div");
    actions.className = "result-actions";
    actions.append(
      actionButton("Open", "open", hit),
      actionButton("Open folder", "open_folder", hit),
      actionButton("Copy path", "copy_path", hit),
    );

    option.append(main, meta, actions);
    option.addEventListener("pointerdown", () => {
      session.select(index);
      updateSelection();
    });
    option.addEventListener("dblclick", () => performAction("open", hit));
    results.append(option);
  });
  const hasResults = session.hits.length > 0;
  emptyState.hidden = hasResults;
  input.setAttribute("aria-expanded", String(hasResults));
  count.textContent = hasResults ? `${session.hits.length} result${session.hits.length === 1 ? "" : "s"}` : "";
  updateSelection();
  recordUxSnapshot(hasResults ? "results" : "empty");
}

function actionButton(label, action, hit) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "action-button";
  button.textContent = label;
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    void performAction(action, hit);
  });
  return button;
}

function updateSelection() {
  results.querySelectorAll(".result").forEach((element, index) => {
    const selected = index === session.selectedIndex;
    element.setAttribute("aria-selected", String(selected));
    if (selected) element.scrollIntoView({ block: "nearest" });
  });
  input.setAttribute(
    "aria-activedescendant",
    session.selectedIndex >= 0 ? `result-${session.selectedIndex}` : "",
  );
}

async function performAction(action, hit = session.selectedHit) {
  if (!hit) return;
  try {
    const result = await invoke("desktop_item_action", {
      request_id: requestId("action"),
      document_id: hit.document_id,
      action,
    });
    setStatus(action === "copy_path" ? "Current path copied" : `Opened ${result.resolved_path}`);
    if (action === "open") await invoke("desktop_hide");
  } catch (error) {
    const code = safeErrorCode(error);
    if (code === "not_found" || code === "item_unavailable") {
      setStatus("This item moved, was deleted, or is currently offline", true);
    } else if (code === "unavailable") {
      serviceAvailable = false;
      setStatus("Search service unavailable", true);
      scheduleReconnect();
    } else {
      setStatus("Could not complete that action", true);
    }
  }
}

function scheduleSearch() {
  if (debounceTimer !== null) window.clearTimeout(debounceTimer);
  const previous = session.currentRequestId;
  const query = input.value.trim();
  if (previous) void invoke("desktop_cancel", { request_id: previous }).catch(() => {});
  if (!query) {
    session.clear();
    renderResults();
    setStatus(idleStatus(serviceAvailable, contentMode), !serviceAvailable);
    return;
  }
  const generation = { ...session.begin(query), contentMode };
  setStatus("Searching…");
  debounceTimer = window.setTimeout(() => runSearch(generation), 90);
}

async function runSearch(generation) {
  debounceTimer = null;
  try {
    const result = await invoke(
      generation.contentMode ? "desktop_content_search" : "desktop_search",
      {
      request_id: generation.requestId,
      query: generation.query,
      },
    );
    if (generation.contentMode) {
      result.response.hits = result.response.hits.map(({ item, rank }) => ({
        ...item,
        rank,
        match_type: "content",
      }));
    }
    const accepted = session.accept(result);
    if (uxEvidenceEnabled) {
      void invoke("desktop_record_ui_search_result", { accepted }).catch(() => {});
    }
    if (!accepted) return;
    serviceAvailable = true;
    renderResults();
    setStatus(
      resultStatus(
        generation.contentMode,
        result.response.hits.length,
        formatDuration(result.response.took_micros),
      ),
    );
  } catch (error) {
    if (!session.isCurrent(generation.requestId)) return;
    const code = safeErrorCode(error);
    if (code === "cancelled" || code === "stale_response") return;
    renderResults();
    if (code === "unavailable" || code === "deadline_exceeded") {
      serviceAvailable = false;
      setStatus("Search service unavailable", true);
      scheduleReconnect();
    } else {
      setStatus("Search could not be completed", true);
    }
  }
}

function selectSearchMode(nextContentMode) {
  if (contentMode === nextContentMode) {
    input.focus();
    return;
  }
  contentMode = nextContentMode;
  const presentation = searchModePresentation(contentMode);
  nameModeButton.setAttribute("aria-pressed", presentation.namePressed);
  contentModeButton.setAttribute("aria-pressed", presentation.contentPressed);
  input.placeholder = presentation.placeholder;
  setStatus(
    contentMode ? "Contents mode · searching inside selected documents" : "Type to search your catalog",
  );
  scheduleSearch();
  input.focus();
}

nameModeButton.addEventListener("click", () => selectSearchMode(false));
contentModeButton.addEventListener("click", () => selectSearchMode(true));

function formatDuration(micros) {
  if (micros < 1000) return `${micros} µs`;
  return `${(micros / 1000).toFixed(1)} ms`;
}

input.addEventListener("input", scheduleSearch);
input.addEventListener("keydown", (event) => {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    session.moveSelection(1);
    updateSelection();
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    session.moveSelection(-1);
    updateSelection();
  } else if (event.key === "Enter") {
    event.preventDefault();
    void performAction("open");
  } else if (event.key === "Escape") {
    event.preventDefault();
    void invoke("desktop_hide");
  }
});

hideButton.addEventListener("click", () => invoke("desktop_hide"));

async function boot() {
  if (!invoke || !listen) {
    setStatus("Desktop bridge unavailable", true);
    return;
  }
  await listen("desktop://focus-search", async (event) => {
    input.focus({ preventScroll: true });
    input.select();
    window.requestAnimationFrame(() => {
      if (document.activeElement === input) {
        void invoke("desktop_ack_focus", { token: event.payload.token });
        recordUxSnapshot("focus");
      }
    });
  });
  await listen("desktop://controlled-ux-query", (event) => {
    if (!uxEvidenceEnabled) return;
    const query = event.payload?.query;
    if (query !== "architecture" && query !== "churn") return;
    input.value = query;
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  try {
    const bootstrap = await invoke("desktop_ready");
    serviceAvailable = bootstrap.service_available;
    searchMode.hidden = !bootstrap.content_search_available;
    uxEvidenceEnabled = bootstrap.ux_evidence_enabled;
    startUiStallMonitor();
    hotkeyLabel.textContent = bootstrap.hotkey.replaceAll("+", " ");
    setStatus(serviceAvailable ? "Type to search your catalog" : "Search service unavailable", !serviceAvailable);
    if (!serviceAvailable) scheduleReconnect();
    recordUxSnapshot("ready");
  } catch {
    setStatus("Search service unavailable", true);
    scheduleReconnect();
  }
}

void boot();
