use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};
use tokio::sync::Mutex as AsyncMutex;

// ---------------------------------------------------------------------------
// macOS App Nap suppression
// ---------------------------------------------------------------------------

// Disable macOS App Nap to prevent the system from pausing our polling timers
// when the window is hidden (minimized to tray). The Rust polling task still
// runs on a tokio runtime that is unaffected by WKWebView throttling, but App
// Nap can also slow the entire process when fully backgrounded.
#[cfg(target_os = "macos")]
fn disable_app_nap() {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        let process_info: id = msg_send![class!(NSProcessInfo), processInfo];
        let reason = NSString::alloc(nil).init_str("Print Agent must keep polling for print jobs");
        // NSActivityUserInitiatedAllowingIdleSystemSleep = 0x00FFFFFFULL
        // This prevents App Nap while still allowing the display to sleep
        let _activity: id = msg_send![process_info,
            beginActivityWithOptions: 0x00FFFFFFu64
            reason: reason
        ];
        // We intentionally never end this activity — it should last the entire app lifetime
    }
}

#[cfg(not(target_os = "macos"))]
fn disable_app_nap() {
    // No-op on non-macOS
}

// ---------------------------------------------------------------------------
// Printer discovery + native printing (unchanged from previous version)
// ---------------------------------------------------------------------------

// Get list of available printers
#[tauri::command]
fn get_printers() -> Result<Vec<String>, String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("lpstat")
            .arg("-e")
            .output()
            .map_err(|e| e.to_string())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let printers: Vec<String> = stdout
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(printers)
    }

    #[cfg(target_os = "windows")]
    {
        // Use PowerShell with WMI (works on all Windows versions including Windows 11)
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "Get-WmiObject -Class Win32_Printer | Select-Object -ExpandProperty Name",
            ])
            .output()
            .map_err(|e| e.to_string())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let printers: Vec<String> = stdout
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(printers)
    }

    #[cfg(target_os = "linux")]
    {
        let output = Command::new("lpstat")
            .arg("-e")
            .output()
            .map_err(|e| e.to_string())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let printers: Vec<String> = stdout
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(printers)
    }
}

// Print result with details
#[derive(serde::Serialize, Clone)]
struct PrintResult {
    success: bool,
    size_kb: usize,
    message: String,
}

// Print a PDF (base64 encoded) — still exposed as a Tauri command so the UI
// can keep it for future tooling, but the polling loop calls the internal
// `print_pdf_internal` directly so it doesn't have to round-trip through
// the WebView.
#[tauri::command]
fn print_pdf(
    pdf_base64: String,
    printer_name: String,
    job_name: String,
) -> Result<PrintResult, String> {
    print_pdf_internal(&pdf_base64, &printer_name, &job_name)
}

fn print_pdf_internal(
    pdf_base64: &str,
    printer_name: &str,
    job_name: &str,
) -> Result<PrintResult, String> {
    // Decode base64 to bytes
    let pdf_bytes = base64::engine::general_purpose::STANDARD
        .decode(pdf_base64)
        .map_err(|e| format!("Failed to decode PDF: {}", e))?;

    let size_kb = pdf_bytes.len() / 1024;

    // Create a temporary file for the PDF
    let temp_dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let pdf_path = temp_dir
        .path()
        .join(format!("{}.pdf", job_name.replace(" ", "_")));

    std::fs::write(&pdf_path, &pdf_bytes)
        .map_err(|e| format!("Failed to write PDF: {}", e))?;

    // Print using system command
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("lp")
            .arg("-d")
            .arg(printer_name)
            .arg("-t")
            .arg(job_name)
            .arg(&pdf_path)
            .output()
            .map_err(|e| format!("Failed to print: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Print failed: {}", stderr));
        }

        return Ok(PrintResult {
            success: true,
            size_kb,
            message: format!("Printed via lp to {}", printer_name),
        });
    }

    #[cfg(target_os = "windows")]
    {
        print_pdf_windows(&pdf_path, printer_name, size_kb)
    }

    #[cfg(target_os = "linux")]
    {
        let output = Command::new("lp")
            .arg("-d")
            .arg(printer_name)
            .arg("-t")
            .arg(job_name)
            .arg(&pdf_path)
            .output()
            .map_err(|e| format!("Failed to print: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Print failed: {}", stderr));
        }

        return Ok(PrintResult {
            success: true,
            size_kb,
            message: format!("Printed via lp to {}", printer_name),
        });
    }
}

// Print PDF using SumatraPDF on Windows (silent, reliable)
#[cfg(target_os = "windows")]
fn print_pdf_windows(
    pdf_path: &std::path::Path,
    printer_name: &str,
    size_kb: usize,
) -> Result<PrintResult, String> {
    // Find SumatraPDF.exe - it's bundled next to the executable
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get executable path: {}", e))?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| "Failed to get executable directory".to_string())?;

    let sumatra_path = exe_dir.join("SumatraPDF.exe");

    if !sumatra_path.exists() {
        return Err(format!(
            "SumatraPDF.exe not found at {:?}. Please ensure it's bundled with the application.",
            sumatra_path
        ));
    }

    // Use SumatraPDF for silent printing
    // Command: SumatraPDF.exe -print-to "printer" -silent file.pdf
    let output = Command::new(&sumatra_path)
        .arg("-print-to")
        .arg(printer_name)
        .arg("-silent")
        .arg(pdf_path)
        .output()
        .map_err(|e| format!("Failed to execute SumatraPDF: {}", e))?;

    if output.status.success() {
        Ok(PrintResult {
            success: true,
            size_kb,
            message: format!("Printed via SumatraPDF to {}", printer_name),
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!(
            "SumatraPDF print failed (exit code {:?}). stdout: {} stderr: {}",
            output.status.code(),
            stdout,
            stderr
        ))
    }
}

// ---------------------------------------------------------------------------
// Polling state + background task
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AgentConfig {
    #[serde(rename = "apiUrl")]
    api_url: String,
    #[serde(rename = "apiKey")]
    api_key: String,
    #[serde(rename = "selectedPrinter")]
    selected_printer: String,
    #[serde(rename = "testMode")]
    test_mode: bool,
}

// Runtime state shared between Tauri commands and the polling task.
// Uses async Mutex for config (held across .await boundaries) and atomic
// flags for the running/stats state so commands can read/write them
// without contending on the mutex.
struct AgentState {
    config: Arc<AsyncMutex<AgentConfig>>,
    is_running: Arc<AtomicBool>,
    jobs_today: Arc<AtomicU32>,
    jobs_total: Arc<AtomicU32>,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            config: Arc::new(AsyncMutex::new(AgentConfig::default())),
            is_running: Arc::new(AtomicBool::new(false)),
            jobs_today: Arc::new(AtomicU32::new(0)),
            jobs_total: Arc::new(AtomicU32::new(0)),
        }
    }
}

// Constants kept in sync with the previous TS implementation. Adjusting here
// changes behaviour for everyone (Mac/Windows/Linux).
const FETCH_TIMEOUT_MS: u64 = 15_000;
const POLL_INTERVAL_MS: u64 = 3_000;
const MAX_CONSECUTIVE_FAILURES: u32 = 5;
const DRIFT_THRESHOLD_MS: u128 = 10_000;

// Wire payloads from the server's /api/print-jobs endpoint
#[derive(Debug, Deserialize)]
struct ServerLabel {
    #[serde(rename = "shopifyOrderName")]
    shopify_order_name: String,
    #[serde(rename = "labelPdf")]
    label_pdf: Option<String>,
    #[serde(rename = "glsTrackingNumber")]
    gls_tracking_number: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ServerJob {
    id: String,
    #[serde(rename = "labelCount")]
    label_count: u32,
    labels: Vec<ServerLabel>,
}

#[derive(Debug, Deserialize)]
struct PollResponse {
    jobs: Vec<ServerJob>,
}

// Events emitted to the UI. The TS side listens for these and updates the
// DOM accordingly — no Tauri commands needed for status updates.
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "kind")]
enum UiEvent {
    #[serde(rename = "status_changed")]
    StatusChanged { status: &'static str }, // "online" | "offline" | "connecting"
    #[serde(rename = "log_added")]
    LogAdded { message: String, level: &'static str }, // "info" | "success" | "error"
    #[serde(rename = "stats_updated")]
    StatsUpdated { today: u32, total: u32 },
}

fn emit(app: &AppHandle, event: UiEvent) {
    if let Err(e) = app.emit("agent_event", event) {
        eprintln!("Failed to emit agent_event: {}", e);
    }
}

fn emit_status(app: &AppHandle, status: &'static str) {
    emit(app, UiEvent::StatusChanged { status });
}

fn emit_log(app: &AppHandle, message: impl Into<String>, level: &'static str) {
    emit(
        app,
        UiEvent::LogAdded {
            message: message.into(),
            level,
        },
    );
}

fn emit_stats(app: &AppHandle, today: u32, total: u32) {
    emit(app, UiEvent::StatsUpdated { today, total });
}

// The polling task. Spawned once per Start; exits when is_running flips off.
// Owns a single reqwest::Client to reuse the TCP connection across polls.
async fn poll_loop(app: AppHandle, state: Arc<AgentState>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            emit_log(&app, format!("Kunne ikke oprette HTTP-klient: {}", e), "error");
            emit_status(&app, "offline");
            state.is_running.store(false, Ordering::Relaxed);
            return;
        }
    };

    let mut consecutive_failures: u32 = 0;
    let mut last_poll_start = Instant::now();

    while state.is_running.load(Ordering::Relaxed) {
        // Drift detection: if we've been gone way longer than the interval
        // (system sleep, suspended runtime, etc.) note it and reset failures.
        let now = Instant::now();
        let elapsed_since_last = now.duration_since(last_poll_start).as_millis();
        if elapsed_since_last > (POLL_INTERVAL_MS as u128 + DRIFT_THRESHOLD_MS) {
            emit_log(
                &app,
                format!(
                    "System var inaktivt i {}s, genoptager polling...",
                    elapsed_since_last / 1000
                ),
                "info",
            );
            consecutive_failures = 0;
        }
        last_poll_start = now;

        // Snapshot config so we don't hold the mutex across the .await
        let cfg = { state.config.lock().await.clone() };

        if cfg.api_url.is_empty() || cfg.api_key.is_empty() {
            emit_log(&app, "Manglende API URL eller API-nøgle", "error");
            emit_status(&app, "offline");
            state.is_running.store(false, Ordering::Relaxed);
            return;
        }

        let url = format!("{}/api/print-jobs", cfg.api_url);
        let req = client.get(&url).header("X-API-Key", &cfg.api_key);

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<PollResponse>().await {
                    Ok(body) => {
                        consecutive_failures = 0;
                        emit_status(&app, "online");

                        if !body.jobs.is_empty() {
                            emit_log(
                                &app,
                                format!("Fandt {} print job(s)", body.jobs.len()),
                                "info",
                            );

                            for job in body.jobs {
                                process_job(&app, &state, &client, &cfg, job).await;
                            }
                        }
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        log_polling_failure(&app, &state, consecutive_failures, &format!("Ugyldig JSON: {}", e));
                    }
                }
            }
            Ok(resp) => {
                consecutive_failures += 1;
                log_polling_failure(
                    &app,
                    &state,
                    consecutive_failures,
                    &format!("HTTP {}", resp.status().as_u16()),
                );
            }
            Err(e) => {
                consecutive_failures += 1;
                let msg = if e.is_timeout() {
                    format!("Timeout (forsøg {})...", consecutive_failures)
                } else {
                    format!("Polling fejl (forsøg {}): {}", consecutive_failures, e)
                };
                log_polling_failure(&app, &state, consecutive_failures, &msg);
            }
        }

        // Sleep until next interval, but check is_running periodically so a
        // Stop click feels responsive instead of waiting up to 3 seconds.
        for _ in 0..30 {
            if !state.is_running.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS / 30)).await;
        }
    }
}

fn log_polling_failure(
    app: &AppHandle,
    _state: &Arc<AgentState>,
    consecutive_failures: u32,
    message: &str,
) {
    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
        emit_status(app, "offline");
        emit_log(app, message, "error");
    } else {
        emit_log(app, message, "info");
    }
}

async fn process_job(
    app: &AppHandle,
    state: &Arc<AgentState>,
    client: &reqwest::Client,
    cfg: &AgentConfig,
    job: ServerJob,
) {
    let mode_label = if cfg.test_mode { " [TEST]" } else { "" };
    emit_log(
        app,
        format!(
            "{} Behandler job {} med {} labels (modtaget: {})...",
            mode_label,
            job.id,
            job.label_count,
            job.labels.len()
        ),
        "info",
    );

    // Mark job as processing on the server
    if let Err(e) = update_job_status(client, cfg, &job.id, "processing", None).await {
        emit_log(
            app,
            format!("{} Kunne ikke markere job som processing: {}", mode_label, e),
            "error",
        );
        return;
    }

    if job.labels.is_empty() {
        let msg = "Ingen labels modtaget fra server - tjek at shipments eksisterer i databasen";
        emit_log(app, format!("{} {}", mode_label, msg), "error");
        let _ = update_job_status(client, cfg, &job.id, "failed", Some(msg)).await;
        return;
    }

    let mut printed_count = 0u32;
    let mut first_print_error: Option<String> = None;

    for label in &job.labels {
        let Some(pdf) = &label.label_pdf else {
            emit_log(
                app,
                format!(
                    "{} Label {} mangler PDF data",
                    mode_label, label.shopify_order_name
                ),
                "error",
            );
            continue;
        };

        emit_log(
            app,
            format!("{} Printer {}...", mode_label, label.shopify_order_name),
            "info",
        );

        if cfg.test_mode {
            let pdf_size_kb = (pdf.len() as f64 * 3.0 / 4.0 / 1024.0) as usize;
            emit_log(
                app,
                format!(
                    "{} Ville printe: {} ({}) - {} KB",
                    mode_label,
                    label.shopify_order_name,
                    label.gls_tracking_number.as_deref().unwrap_or(""),
                    pdf_size_kb
                ),
                "success",
            );
            printed_count += 1;
        } else {
            let job_name = format!("GLS Label - {}", label.shopify_order_name);
            let pdf_owned = pdf.clone();
            let printer_owned = cfg.selected_printer.clone();
            // Run native printing on a blocking thread so we don't block the
            // tokio reactor while `lp` / SumatraPDF runs.
            let print_outcome: Result<PrintResult, String> =
                match tokio::task::spawn_blocking(move || {
                    print_pdf_internal(&pdf_owned, &printer_owned, &job_name)
                })
                .await
                {
                    Ok(inner) => inner,
                    Err(join_err) => Err(format!("Print task panicked: {}", join_err)),
                };

            match print_outcome {
                Ok(result) => {
                    emit_log(
                        app,
                        format!(
                            "Printet: {} ({}) - {} KB",
                            label.shopify_order_name,
                            label.gls_tracking_number.as_deref().unwrap_or(""),
                            result.size_kb
                        ),
                        "success",
                    );
                    printed_count += 1;
                }
                Err(err_msg) => {
                    emit_log(
                        app,
                        format!("Print fejl for {}: {}", label.shopify_order_name, err_msg),
                        "error",
                    );
                    first_print_error = Some(err_msg);
                    break;
                }
            }
        }
    }

    if let Some(err) = first_print_error {
        let _ = update_job_status(client, cfg, &job.id, "failed", Some(&err)).await;
        return;
    }

    if printed_count == 0 {
        let msg = "Ingen labels blev printet - alle manglede PDF data";
        emit_log(app, format!("{} {}", mode_label, msg), "error");
        let _ = update_job_status(client, cfg, &job.id, "failed", Some(msg)).await;
        return;
    }

    if let Err(e) = update_job_status(client, cfg, &job.id, "completed", None).await {
        emit_log(
            app,
            format!("{} Kunne ikke markere job som completed: {}", mode_label, e),
            "error",
        );
    }

    let new_today = state.jobs_today.fetch_add(printed_count, Ordering::Relaxed) + printed_count;
    let new_total = state.jobs_total.fetch_add(printed_count, Ordering::Relaxed) + printed_count;
    emit_stats(app, new_today, new_total);

    emit_log(
        app,
        format!(
            "{} Job {} fuldført ({} labels)",
            mode_label, job.id, printed_count
        ),
        "success",
    );
}

async fn update_job_status(
    client: &reqwest::Client,
    cfg: &AgentConfig,
    job_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let url = format!("{}/api/print-jobs", cfg.api_url);
    let body = serde_json::json!({
        "jobId": job_id,
        "status": status,
        "error": error,
    });

    let resp = client
        .put(&url)
        .header("X-API-Key", &cfg.api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status_code = resp.status().as_u16();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {} - {}", status_code, body_text));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands (public surface to the UI)
// ---------------------------------------------------------------------------

#[tauri::command]
async fn update_config(
    state: State<'_, Arc<AgentState>>,
    config: AgentConfig,
) -> Result<(), String> {
    let mut cfg = state.config.lock().await;
    *cfg = config;
    Ok(())
}

#[tauri::command]
async fn start_polling(
    app: AppHandle,
    state: State<'_, Arc<AgentState>>,
) -> Result<(), String> {
    // Atomically flip running on; if already on, no-op
    let was_running = state.is_running.swap(true, Ordering::Relaxed);
    if was_running {
        return Ok(());
    }

    emit_status(&app, "connecting");

    let state_clone = Arc::clone(state.inner());
    let app_clone = app.clone();
    tokio::spawn(async move {
        poll_loop(app_clone, state_clone).await;
    });

    Ok(())
}

#[tauri::command]
async fn stop_polling(
    app: AppHandle,
    state: State<'_, Arc<AgentState>>,
) -> Result<(), String> {
    state.is_running.store(false, Ordering::Relaxed);
    emit_status(&app, "offline");
    Ok(())
}

#[tauri::command]
async fn set_stats(
    state: State<'_, Arc<AgentState>>,
    today: u32,
    total: u32,
) -> Result<(), String> {
    state.jobs_today.store(today, Ordering::Relaxed);
    state.jobs_total.store(total, Ordering::Relaxed);
    Ok(())
}

// ---------------------------------------------------------------------------
// App bootstrap
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(Arc::new(AgentState::default()))
        .setup(|app| {
            // Disable App Nap so macOS doesn't pause our polling when minimized to tray
            disable_app_nap();

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Create system tray menu
            let show_item = MenuItem::with_id(app, "show", "Åbn", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Afslut", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            // Load tray icon from app icons
            let icon = Image::from_path("icons/32x32.png")
                .unwrap_or_else(|_| Image::from_bytes(include_bytes!("../icons/32x32.png")).unwrap());

            // Create system tray
            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .tooltip("GLS Print Agent")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide window instead of closing when X is clicked — polling
            // continues in the Rust task either way, but keeping the window
            // around means the user can re-open instantly from the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_printers,
            print_pdf,
            update_config,
            start_polling,
            stop_polling,
            set_stats,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
