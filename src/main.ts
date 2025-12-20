import { invoke } from "@tauri-apps/api/core";
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

// State
let config: Config = {
  apiUrl: "",
  apiKey: "",
  selectedPrinter: "",
  testMode: false,
};
let isRunning = false;
let pollInterval: number | null = null;
let store: Store;
let jobsToday = 0;
let jobsTotal = 0;

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

// Initialize
async function init() {
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
  startStopBtn.textContent = "Stop";
  startStopBtn.classList.remove("secondary");
  startStopBtn.classList.add("danger");
  setStatus("connecting");

  if (config.testMode) {
    log("Starter polling i TEST-TILSTAND (printer ikke)...", "info");
  } else {
    log("Starter polling...", "info");
  }

  // Poll immediately, then every 3 seconds
  pollForJobs();
  pollInterval = window.setInterval(pollForJobs, 3000);
}

function stopPolling() {
  isRunning = false;
  startStopBtn.textContent = "Start";
  startStopBtn.classList.remove("danger");
  startStopBtn.classList.add("secondary");
  setStatus("offline");

  if (pollInterval) {
    clearInterval(pollInterval);
    pollInterval = null;
  }

  log("Polling stoppet", "info");
}

async function pollForJobs() {
  try {
    const response = await fetch(`${config.apiUrl}/api/print-jobs`, {
      headers: {
        "X-API-Key": config.apiKey,
      },
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }

    setStatus("online");
    const data = await response.json();

    if (data.jobs && data.jobs.length > 0) {
      log(`Fandt ${data.jobs.length} print job(s)`, "info");

      for (const job of data.jobs) {
        await processJob(job);
      }
    }
  } catch (error) {
    setStatus("offline");
    log(`Polling fejl: ${error}`, "error");
  }
}

async function processJob(job: PrintJob) {
  const modeLabel = config.testMode ? " [TEST]" : "";
  log(`${modeLabel} Behandler job ${job.id} med ${job.labelCount} labels...`, "info");

  try {
    // Mark job as processing
    await updateJobStatus(job.id, "processing");

    // Print each label
    for (const label of job.labels) {
      if (!label.labelPdf) {
        log(`${modeLabel} Label ${label.shopifyOrderName} mangler PDF`, "error");
        continue;
      }

      if (config.testMode) {
        // Test mode: just log what would be printed
        const pdfSizeKb = Math.round((label.labelPdf.length * 3) / 4 / 1024);
        log(`${modeLabel} Ville printe: ${label.shopifyOrderName} (${label.glsTrackingNumber}) - ${pdfSizeKb} KB`, "success");
      } else {
        // Real mode: actually print
        await printPdf(label.labelPdf, label.shopifyOrderName);
        log(`Printet: ${label.shopifyOrderName} (${label.glsTrackingNumber})`, "success");
      }
    }

    // Mark job as completed
    await updateJobStatus(job.id, "completed");

    // Update stats
    jobsToday += job.labelCount;
    jobsTotal += job.labelCount;
    updateStats();
    await saveStats();

    log(`${modeLabel} Job ${job.id} fuldført`, "success");
  } catch (error) {
    log(`${modeLabel} Job fejl: ${error}`, "error");
    await updateJobStatus(job.id, "failed", String(error));
  }
}

async function printPdf(base64Pdf: string, orderName: string) {
  await invoke("print_pdf", {
    pdfBase64: base64Pdf,
    printerName: config.selectedPrinter,
    jobName: `GLS Label - ${orderName}`,
  });
}

async function updateJobStatus(jobId: string, status: string, error?: string) {
  try {
    await fetch(`${config.apiUrl}/api/print-jobs`, {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        "X-API-Key": config.apiKey,
      },
      body: JSON.stringify({ jobId, status, error }),
    });
  } catch (e) {
    console.error("Failed to update job status:", e);
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
