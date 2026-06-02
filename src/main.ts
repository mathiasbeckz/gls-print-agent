import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { Store } from "@tauri-apps/plugin-store";

// Polling, fetching and printing all live in Rust (src-tauri/src/lib.rs) now.
// This file is only responsible for:
//   - Loading / persisting the saved config and stats via tauri-plugin-store
//   - Forwarding Start / Stop / save-config button clicks to Rust commands
//   - Listening for "agent_event" events emitted by the Rust polling task
//     and updating the DOM (status badge, activity log, stats)
//
// The previous version's setTimeout-based poll loop ran inside the WebView
// and was throttled to a crawl by WKWebView whenever the window was hidden
// (minimized, behind other windows, system idle). That is why the agent
// went offline on macOS until the window was clicked. Moving the poll
// loop into a tokio task on the Rust side avoids that throttling entirely.

// Types
interface Config {
  apiUrl: string;
  apiKey: string;
  selectedPrinter: string;
  testMode: boolean;
  printDarkness: string; // "" = printer default, otherwise "1".."30"
}

type AgentEvent =
  | { kind: "status_changed"; status: "online" | "offline" | "connecting" }
  | { kind: "log_added"; message: string; level: "info" | "success" | "error" }
  | { kind: "stats_updated"; today: number; total: number };

// State
let config: Config = {
  apiUrl: "",
  apiKey: "",
  selectedPrinter: "",
  testMode: false,
  printDarkness: "",
};
let isRunning = false;
let store: Store;
let jobsToday = 0;
let jobsTotal = 0;

// Elements
const statusEl = document.getElementById("status")!;
const apiUrlInput = document.getElementById("api-url") as HTMLInputElement;
const apiKeyInput = document.getElementById("api-key") as HTMLInputElement;
const printerSelect = document.getElementById("printer-select") as HTMLSelectElement;
const testModeCheckbox = document.getElementById("test-mode") as HTMLInputElement;
const darknessInput = document.getElementById("darkness") as HTMLInputElement;
const saveConfigBtn = document.getElementById("save-config")!;
const startStopBtn = document.getElementById("start-stop")!;
const activityLog = document.getElementById("activity-log")!;
const jobsTodayEl = document.getElementById("jobs-today")!;
const jobsTotalEl = document.getElementById("jobs-total")!;
const appVersionEl = document.getElementById("app-version")!;

async function init() {
  // Show app version
  try {
    const version = await getVersion();
    appVersionEl.textContent = `v${version}`;
  } catch {
    appVersionEl.textContent = "v?";
  }

  store = await Store.load("config.json");

  // Load saved config
  const savedConfig = await store.get<Config>("config");
  if (savedConfig) {
    config = { printDarkness: "", ...savedConfig };
    apiUrlInput.value = config.apiUrl;
    apiKeyInput.value = config.apiKey;
    testModeCheckbox.checked = config.testMode || false;
    darknessInput.value = config.printDarkness || "";
    // Push the loaded config into Rust state immediately so a Start click
    // doesn't need to wait for the save button.
    await invoke("update_config", { config });
  }

  // Load stats and sync to Rust so the counters survive restarts
  const savedStats = await store.get<{ today: number; total: number }>("stats");
  if (savedStats) {
    jobsToday = savedStats.today;
    jobsTotal = savedStats.total;
    updateStats();
    await invoke("set_stats", { today: jobsToday, total: jobsTotal });
  }

  await loadPrinters();

  // Listen for events emitted by the Rust polling task
  await listen<AgentEvent>("agent_event", (event) => {
    handleAgentEvent(event.payload);
  });

  // UI event handlers
  saveConfigBtn.addEventListener("click", saveConfig);
  startStopBtn.addEventListener("click", toggleRunning);

  log("Print Agent klar", "info");
}

function handleAgentEvent(event: AgentEvent) {
  switch (event.kind) {
    case "status_changed":
      setStatus(event.status);
      break;
    case "log_added":
      log(event.message, event.level);
      break;
    case "stats_updated":
      jobsToday = event.today;
      jobsTotal = event.total;
      updateStats();
      saveStats().catch(() => {/* non-fatal */});
      break;
  }
}

async function loadPrinters() {
  try {
    const printers: string[] = await invoke("get_printers");
    printerSelect.innerHTML = '<option value="">Vælg printer...</option>';

    for (const printer of printers) {
      const option = document.createElement("option");
      option.value = printer;
      option.textContent = printer;
      if (printer === config.selectedPrinter) {
        option.selected = true;
      }
      printerSelect.appendChild(option);
    }
  } catch (error) {
    log(`Kunne ikke hente printere: ${error}`, "error");
  }
}

async function saveConfig() {
  config.apiUrl = apiUrlInput.value.replace(/\/$/, ""); // Remove trailing slash
  config.apiKey = apiKeyInput.value;
  config.selectedPrinter = printerSelect.value;
  config.testMode = testModeCheckbox.checked;
  // Normalise darkness: empty / 0 / out-of-range → use printer default
  const darknessRaw = darknessInput.value.trim();
  const darknessNum = parseInt(darknessRaw, 10);
  config.printDarkness =
    darknessRaw && !isNaN(darknessNum) && darknessNum >= 1 && darknessNum <= 30
      ? String(darknessNum)
      : "";

  await store.set("config", config);
  await store.save();
  await invoke("update_config", { config });

  log("Konfiguration gemt" + (config.testMode ? " (test-tilstand)" : ""), "success");
}

async function toggleRunning() {
  if (isRunning) {
    await stopPolling();
  } else {
    await startPolling();
  }
}

async function startPolling() {
  if (!config.apiUrl || !config.apiKey) {
    log("Udfyld API URL og API nøgle først", "error");
    return;
  }

  if (!config.selectedPrinter && !config.testMode) {
    log("Vælg en printer først (eller aktiver test-tilstand)", "error");
    return;
  }

  // Ensure Rust has the latest config before starting
  await invoke("update_config", { config });

  isRunning = true;
  startStopBtn.textContent = "Stop";
  startStopBtn.classList.remove("secondary");
  startStopBtn.classList.add("danger");
  setStatus("connecting");

  if (config.testMode) {
    log("Starter polling i TEST-TILSTAND (printer ikke)...", "info");
  } else {
    log("Starter polling...", "info");
  }

  await invoke("start_polling");
}

async function stopPolling() {
  await invoke("stop_polling");
  isRunning = false;
  startStopBtn.textContent = "Start";
  startStopBtn.classList.remove("danger");
  startStopBtn.classList.add("secondary");
  setStatus("offline");
  log("Polling stoppet", "info");
}

function setStatus(status: "online" | "offline" | "connecting") {
  statusEl.className = `status ${status}`;
  statusEl.textContent =
    status === "online"
      ? "Forbundet"
      : status === "connecting"
        ? "Forbinder..."
        : "Ikke forbundet";
}

function log(message: string, type: "info" | "success" | "error") {
  const time = new Date().toLocaleTimeString("da-DK");
  const item = document.createElement("p");
  item.className = `log-item ${type}`;
  item.innerHTML = `<span class="time">${time}</span>${message}`;

  // Add at top
  activityLog.insertBefore(item, activityLog.firstChild);

  // Keep max 50 entries
  while (activityLog.children.length > 50) {
    activityLog.removeChild(activityLog.lastChild!);
  }
}

function updateStats() {
  jobsTodayEl.textContent = String(jobsToday);
  jobsTotalEl.textContent = String(jobsTotal);
}

async function saveStats() {
  await store.set("stats", { today: jobsToday, total: jobsTotal });
  await store.save();
}

// Initialize when DOM is ready
document.addEventListener("DOMContentLoaded", init);
