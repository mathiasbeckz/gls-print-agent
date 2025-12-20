use std::process::Command;
use tauri::Manager;
use base64::Engine;

// Get list of available printers
#[tauri::command]
fn get_printers() -> Result<Vec<String>, String> {
    #[cfg(target_os = "macos")]
    {
        // Use lpstat -e which just lists printer names (language independent)
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
        let output = Command::new("powershell")
            .args(["-Command", "Get-Printer | Select-Object -ExpandProperty Name"])
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
        // Use lpstat -e which just lists printer names (language independent)
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

    // Create a temporary file
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
        let pdf_path_str = pdf_path.display().to_string();

        // Method 1: Try SumatraPDF (best for silent printing)
        // Check common installation paths
        let sumatra_paths = [
            r"C:\Program Files\SumatraPDF\SumatraPDF.exe",
            r"C:\Program Files (x86)\SumatraPDF\SumatraPDF.exe",
            &format!(r"{}\AppData\Local\SumatraPDF\SumatraPDF.exe", std::env::var("USERPROFILE").unwrap_or_default()),
        ];

        for sumatra_path in &sumatra_paths {
            if std::path::Path::new(sumatra_path).exists() {
                let output = Command::new(sumatra_path)
                    .args([
                        "-print-to", &printer_name,
                        "-silent",
                        "-print-settings", "fit",
                        &pdf_path_str,
                    ])
                    .output()
                    .map_err(|e| format!("SumatraPDF failed: {}", e))?;

                if output.status.success() {
                    return Ok(PrintResult {
                        success: true,
                        size_kb,
                        message: format!("Printed via SumatraPDF to {}", printer_name),
                    });
                }
            }
        }

        // Method 2: Try Adobe Reader if installed
        let adobe_paths = [
            r"C:\Program Files\Adobe\Acrobat Reader DC\Reader\AcroRd32.exe",
            r"C:\Program Files (x86)\Adobe\Acrobat Reader DC\Reader\AcroRd32.exe",
            r"C:\Program Files\Adobe\Reader 11.0\Reader\AcroRd32.exe",
            r"C:\Program Files (x86)\Adobe\Reader 11.0\Reader\AcroRd32.exe",
        ];

        for adobe_path in &adobe_paths {
            if std::path::Path::new(adobe_path).exists() {
                let output = Command::new(adobe_path)
                    .args([
                        "/t",  // Print and exit
                        &pdf_path_str,
                        &printer_name,
                    ])
                    .output()
                    .map_err(|e| format!("Adobe Reader failed: {}", e))?;

                // Adobe Reader returns quickly, give it time to spool
                std::thread::sleep(std::time::Duration::from_secs(3));

                if output.status.success() {
                    return Ok(PrintResult {
                        success: true,
                        size_kb,
                        message: format!("Printed via Adobe Reader to {}", printer_name),
                    });
                }
            }
        }

        // Method 3: Try Foxit Reader
        let foxit_paths = [
            r"C:\Program Files\Foxit Software\Foxit PDF Reader\FoxitPDFReader.exe",
            r"C:\Program Files (x86)\Foxit Software\Foxit PDF Reader\FoxitPDFReader.exe",
        ];

        for foxit_path in &foxit_paths {
            if std::path::Path::new(foxit_path).exists() {
                let output = Command::new(foxit_path)
                    .args([
                        "/t",
                        &pdf_path_str,
                        &printer_name,
                    ])
                    .output()
                    .map_err(|e| format!("Foxit Reader failed: {}", e))?;

                std::thread::sleep(std::time::Duration::from_secs(3));

                if output.status.success() {
                    return Ok(PrintResult {
                        success: true,
                        size_kb,
                        message: format!("Printed via Foxit Reader to {}", printer_name),
                    });
                }
            }
        }

        // No PDF reader found
        return Err("Ingen PDF-læser fundet. Installer SumatraPDF fra https://www.sumatrapdfreader.org/download-free-pdf-viewer".to_string());
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_printers, print_pdf])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
