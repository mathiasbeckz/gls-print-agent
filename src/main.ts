import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { Store } from "@tauri-apps/plugin-store";

// Types
interface PrintJob {
  id: string;
  labelCount: number;
  createdAt: string;
  labels: Array<{
    id: string;
    shopifyOrderName: string;
    labelPdf: string;
    glsTrackingNumber: string;
  }>;
}

interface Config {
  apiUrl: string;
  apiKey: string;
  selectedPrinter: string;
  testMode: boolean;
}

// Constants
const FETCH_TIMEOUT_MS = 15000; // 15 seconds - abort hung requests
const POLL_INTERVAL_MS = 3000; // 3 seconds between polls
const MAX_CONSECUTIVE_FAILURES = 5; // Only show offline after this many failures
const DRIFT_THRESHOLD_MS = 10000; // If poll is delayed by more than 10s, system likely slept

// State
let config: Config = {
  apiUrl: "",
  apiKey: "",
  selectedPrinter: "",
  testMode: false,
};
let isRunning = false;
let pollTimeout: number | null = null;
let store: Store;
let jobsToday = 0;
let jobsTotal = 0;
let consecutiveFailures = 0;
let lastPollTime = 0;

// Elements
const statusEl = document.getElementById("status")!;
const apiUrlInput = document.getElementById("api-url") as HTMLInputElement;
const apiKeyInput = document.getElementById("api-key") as HTMLInputElement;
const printerSelect = document.getElementById("printer-select") as HTMLSelectElement;
const testModeCheckbox = document.getElementById("test-mode") as HTMLInputElement;
const saveConfigBtn = document.getElementById("save-config")!;
const startStopBtn = document.getElementById("start-stop")!;
const activityLog = document.getElementById("activity-log")!;
const jobsTodayEl = document.getElementById("jobs-today")!;
const jobsTotalEl = document.getElementById("jobs-total")!;
const appVersionEl = document.getElementById("app-version")!;

// Fetch with timeout - prevents hung requests from deadlocking the agent
function fetchWithTimeout(url: string, options: RequestInit = {}): Promise<Response> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);

  return fetch(url, { ...options, signal: controller.signal }).finally(() => {
    clearTimeout(timeoutId);
  });
}

// Initialize
async function init() {
  // Show app version
  try {
    const version = await getVersion();
    appVersionEl.textContent = `v${version}`;
  } catch {
    appVersionEl.textContent = "v?";
  }
  // Initialize store
  store = await Store.load("config.json");

  // Load saved config
  const savedConfig = await store.get<Config>("config");
  if (savedConfig) {
    config = savedConfig;
    apiUrlInput.value = config.apiUrl;
    apiKeyInput.value = config.apiKey;
    testModeCheckbox.checked = config.testMode || false;
  }

  // Load stats
  const savedStats = await store.get<{ today: number; total: number }>("stats");
  if (savedStats) {
    jobsToday = savedStats.today;
    jobsTotal = savedStats.total;
    updateStats();
  }

  // Load printers
  await loadPrinters();

  // Set up event listeners
  saveConfigBtn.addEventListener("click", saveConfig);
  startStopBtn.addEventListener("click", toggleRunning);

  // Detect system wake / tab becoming visible — immediately re-poll
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && isRunning) {
      log("App blev synlig igen, tjekker forbindelse...", "info");
      resetPollTimer();
    }
  });

  // Detect network coming back online
  window.addEventListener("online", () => {
    if (isRunning) {
      log("Netværk genoprettet, genopretter forbindelse...", "info");
      consecutiveFailures = 0;
      resetPollTimer();
    }
  });

  window.addEventListener("offline", () => {
    if (isRunning) {
      log("Netværk mistet", "error");
    }
  });

  log("Print Agent klar", "info");
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

  await store.set("config", config);
  await store.save();

  log("Konfiguration gemt" + (config.testMode ? " (test-tilstand)" : ""), "success");
}

function toggleRunning() {
  if (isRunning) {
    stopPolling();
  } else {
    startPolling();
  }
}

function startPolling() {
  if (!config.apiUrl || !config.apiKey) {
    log("Udfyld API URL og API nøgle først", "error");
    return;
  }

  if (!config.selectedPrinter && !config.testMode) {
    log("Vælg en printer først (eller aktiver test-tilstand)", "error");
    return;
  }

  isRunning = true;
  consecutiveFailures = 0;
  startStopBtn.textContent = "Stop";
  startStopBtn.classList.remove("secondary");
  startStopBtn.classList.add("danger");
  setStatus("connecting");

  if (config.testMode) {
    log("Starter polling i TEST-TILSTAND (printer ikke)...", "info");
  } else {
    log("Starter polling...", "info");
  }

  // Poll immediately - next poll is scheduled after this one completes
  lastPollTime = Date.now();
  pollForJobs();
}

function stopPolling() {
  isRunning = false;
  consecutiveFailures = 0;
  startStopBtn.textContent = "Start";
  startStopBtn.classList.remove("danger");
  startStopBtn.classList.add("secondary");
  setStatus("offline");

  if (pollTimeout) {
    clearTimeout(pollTimeout);
    pollTimeout = null;
  }

  log("Polling stoppet", "info");
}

// Cancel any pending poll and poll immediately
function resetPollTimer() {
  if (!isRunning) return;
  if (pollTimeout) {
    clearTimeout(pollTimeout);
    pollTimeout = null;
  }
  pollForJobs();
}

function scheduleNextPoll() {
  if (!isRunning) return;
  lastPollTime = Date.now();
  pollTimeout = window.setTimeout(() => {
    // Detect timer drift (system sleep/App Nap)
    const elapsed = Date.now() - lastPollTime;
    if (elapsed > POLL_INTERVAL_MS + DRIFT_THRESHOLD_MS) {
      log(`System var inaktivt i ${Math.round(elapsed / 1000)}s, genoptager polling...`, "info");
      consecutiveFailures = 0; // Reset failures after wake
    }
    pollForJobs();
  }, POLL_INTERVAL_MS);
}

async function pollForJobs() {
  try {
    const response = await fetchWithTimeout(`${config.apiUrl}/api/print-jobs`, {
      headers: {
        "X-API-Key": config.apiKey,
      },
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }

    // Success — reset failure counter and set online
    consecutiveFailures = 0;
    setStatus("online");
    const data = await response.json();

    if (data.jobs && data.jobs.length > 0) {
      log(`Fandt ${data.jobs.length} print job(s)`, "info");

      for (const job of data.jobs) {
        await processJob(job);
      }
    }
  } catch (error) {
    consecutiveFailures++;

    const message = error instanceof DOMException && error.name === "AbortError"
      ? `Timeout (forsøg ${consecutiveFailures})...`
      : `Polling fejl (forsøg ${consecutiveFailures}): ${error}`;

    // Only show as error and set offline after multiple consecutive failures
    if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
      setStatus("offline");
      log(message, "error");
    } else {
      // Keep current status (online/connecting) during transient failures
      log(message, "info");
    }
  } finally {
    // Schedule next poll AFTER this one finishes - no overlap possible
    scheduleNextPoll();
  }
}

async function processJob(job: PrintJob) {
  const modeLabel = config.testMode ? " [TEST]" : "";
  log(`${modeLabel} Behandler job ${job.id} med ${job.labelCount} labels (modtaget: ${job.labels.length})...`, "info");

  try {
    // Mark job as processing
    await updateJobStatus(job.id, "processing");

    // Check if labels array is empty
    if (job.labels.length === 0) {
      throw new Error("Ingen labels modtaget fra server - tjek at shipments eksisterer i databasen");
    }

    let printedCount = 0;

    // Print each label
    for (const label of job.labels) {
      if (!label.labelPdf) {
        log(`${modeLabel} Label ${label.shopifyOrderName} mangler PDF data`, "error");
        continue;
      }

      log(`${modeLabel} Printer ${label.shopifyOrderName}...`, "info");

      if (config.testMode) {
        // Test mode: just log what would be printed
        const pdfSizeKb = Math.round((label.labelPdf.length * 3) / 4 / 1024);
        log(`${modeLabel} Ville printe: ${label.shopifyOrderName} (${label.glsTrackingNumber}) - ${pdfSizeKb} KB`, "success");
        printedCount++;
      } else {
        // Real mode: actually print
        try {
          const result = await printPdf(label.labelPdf, label.shopifyOrderName);
          log(`Printet: ${label.shopifyOrderName} (${label.glsTrackingNumber}) - ${result.size_kb} KB`, "success");
          printedCount++;
        } catch (printError) {
          log(`Print fejl for ${label.shopifyOrderName}: ${printError}`, "error");
          throw printError;
        }
      }
    }

    // Only mark as completed if we actually printed something
    if (printedCount === 0) {
      throw new Error("Ingen labels blev printet - alle manglede PDF data");
    }

    // Mark job as completed
    await updateJobStatus(job.id, "completed");

    // Update stats with actual printed count
    jobsToday += printedCount;
    jobsTotal += printedCount;
    updateStats();
    await saveStats();

    log(`${modeLabel} Job ${job.id} fuldført (${printedCount} labels)`, "success");
  } catch (error) {
    log(`${modeLabel} Job fejl: ${error}`, "error");
    await updateJobStatus(job.id, "failed", String(error));
  }
}

interface PrintResult {
  success: boolean;
  size_kb: number;
  message: string;
}

async function printPdf(base64Pdf: string, orderName: string): Promise<PrintResult> {
  return await invoke("print_pdf", {
    pdfBase64: base64Pdf,
    printerName: config.selectedPrinter,
    jobName: `GLS Label - ${orderName}`,
  });
}

async function updateJobStatus(jobId: string, status: string, error?: string) {
  const response = await fetchWithTimeout(`${config.apiUrl}/api/print-jobs`, {
    method: "PUT",
    headers: {
      "Content-Type": "application/json",
      "X-API-Key": config.apiKey,
    },
    body: JSON.stringify({ jobId, status, error }),
  });

  if (!response.ok) {
    const errorText = await response.text().catch(() => "Unknown error");
    throw new Error(`Failed to update job status to "${status}": HTTP ${response.status} - ${errorText}`);
  }
}

function setStatus(status: "online" | "offline" | "connecting") {
  statusEl.className = `status ${status}`;
  statusEl.textContent =
    status === "online" ? "Forbundet" :
    status === "connecting" ? "Forbinder..." : "Ikke forbundet";
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
