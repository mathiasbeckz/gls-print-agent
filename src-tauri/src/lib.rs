use std::process::Command;
use base64::Engine;
use tauri::{
    Manager,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    menu::{Menu, MenuItem},
    image::Image,
};

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
                "Get-WmiObject -Class Win32_Printer | Select-Object -ExpandProperty Name"
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
#[derive(serde::Serialize)]
struct PrintResult {
    success: bool,
    size_kb: usize,
    message: String,
}

// Print a PDF (base64 encoded)
#[tauri::command]
fn print_pdf(pdf_base64: String, printer_name: String, job_name: String) -> Result<PrintResult, String> {
    // Decode base64 to bytes
    let pdf_bytes = base64::engine::general_purpose::STANDARD
        .decode(&pdf_base64)
        .map_err(|e| format!("Failed to decode PDF: {}", e))?;

    let size_kb = pdf_bytes.len() / 1024;

    // Create a temporary file for the PDF
    let temp_dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let pdf_path = temp_dir.path().join(format!("{}.pdf", job_name.replace(" ", "_")));

    std::fs::write(&pdf_path, &pdf_bytes)
        .map_err(|e| format!("Failed to write PDF: {}", e))?;

    // Print using system command
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("lp")
            .arg("-d")
            .arg(&printer_name)
            .arg("-t")
            .arg(&job_name)
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
        print_pdf_windows(&pdf_path, &printer_name, size_kb)
    }

    #[cfg(target_os = "linux")]
    {
        let output = Command::new("lp")
            .arg("-d")
            .arg(&printer_name)
            .arg("-t")
            .arg(&job_name)
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
fn print_pdf_windows(pdf_path: &std::path::Path, printer_name: &str, size_kb: usize) -> Result<PrintResult, String> {
    // Find SumatraPDF.exe - it's bundled next to the executable
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get executable path: {}", e))?;
    let exe_dir = exe_path.parent()
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .setup(|app| {
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
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
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
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
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
            // Hide window instead of closing when X is clicked
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![get_printers, print_pdf])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
