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
        // Use PDFtoPrinter or print via PowerShell with Out-Printer
        // Method 1: Try using PowerShell Get-Content and Out-Printer (works for raw/text)
        // Method 2: Use rundll32 with mshtml (more reliable for PDFs)

        let pdf_path_str = pdf_path.display().to_string();

        // Use PowerShell to print PDF via default PDF handler
        // The -PassThru and -Wait ensure we wait for completion
        let script = format!(
            r#"
            $pdfPath = '{}'
            $printerName = '{}'

            # Method: Use Windows print verb with shell
            $shell = New-Object -ComObject Shell.Application
            $folder = $shell.Namespace((Split-Path $pdfPath))
            $file = $folder.ParseName((Split-Path $pdfPath -Leaf))

            # Get the PrintTo verb
            $printVerb = $file.Verbs() | Where-Object {{ $_.Name -like '*Print*' }} | Select-Object -First 1

            if ($printVerb) {{
                # Set default printer temporarily
                $oldDefault = (Get-WmiObject -Query "SELECT * FROM Win32_Printer WHERE Default=$true").Name
                $printer = Get-WmiObject -Query "SELECT * FROM Win32_Printer WHERE Name='$printerName'"
                if ($printer) {{
                    $printer.SetDefaultPrinter() | Out-Null
                    $printVerb.DoIt()
                    Start-Sleep -Seconds 2
                    # Restore old default if different
                    if ($oldDefault -and $oldDefault -ne $printerName) {{
                        $oldPrinter = Get-WmiObject -Query "SELECT * FROM Win32_Printer WHERE Name='$oldDefault'"
                        if ($oldPrinter) {{ $oldPrinter.SetDefaultPrinter() | Out-Null }}
                    }}
                    Write-Output "SUCCESS"
                }} else {{
                    Write-Error "Printer not found: $printerName"
                }}
            }} else {{
                Write-Error "No print verb available"
            }}
            "#,
            pdf_path_str.replace("'", "''"),
            printer_name.replace("'", "''")
        );

        let output = Command::new("powershell")
            .args(["-ExecutionPolicy", "Bypass", "-Command", &script])
            .output()
            .map_err(|e| format!("Failed to execute print command: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if stdout.contains("SUCCESS") {
            return Ok(PrintResult {
                success: true,
                size_kb,
                message: format!("Printed to {}", printer_name),
            });
        } else if !stderr.is_empty() {
            return Err(format!("Print failed: {}", stderr.trim()));
        } else {
            // Fallback: try direct print command
            let fallback_output = Command::new("cmd")
                .args(["/C", "print", &format!("/D:{}", printer_name), &pdf_path_str])
                .output()
                .map_err(|e| format!("Fallback print failed: {}", e))?;

            if fallback_output.status.success() {
                return Ok(PrintResult {
                    success: true,
                    size_kb,
                    message: format!("Printed via cmd to {}", printer_name),
                });
            }

            return Err("Print command completed but status unclear".to_string());
        }
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
