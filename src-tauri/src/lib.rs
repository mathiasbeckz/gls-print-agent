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
        let pdf_path_str = pdf_path.display().to_string().replace("\\", "/");

        // Use Windows 10+ built-in PDF rendering via PowerShell
        // This renders PDF to image and prints using System.Drawing.Printing
        let script = format!(
            r#"
Add-Type -AssemblyName System.Drawing

# Load Windows Runtime for PDF rendering
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Foundation.IAsyncOperation`1, Windows.Foundation, ContentType=WindowsRuntime]
$null = [Windows.Storage.Streams.IRandomAccessStream, Windows.Storage, ContentType=WindowsRuntime]
$null = [Windows.Data.Pdf.PdfDocument, Windows.Data.Pdf, ContentType=WindowsRuntime]

function Await($WinRtTask, $ResultType) {{
    $asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{
        $_.Name -eq 'AsTask' -and
        $_.GetParameters().Count -eq 1 -and
        $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
    }} | Select-Object -First 1
    $asTaskT = $asTask.MakeGenericMethod($ResultType)
    $task = $asTaskT.Invoke($null, @($WinRtTask))
    $task.Wait()
    return $task.Result
}}

try {{
    $pdfPath = '{}'
    $printerName = '{}'

    # Open PDF file
    $file = [System.IO.File]::OpenRead($pdfPath)
    $randomAccessStream = [System.IO.WindowsRuntimeStreamExtensions]::AsRandomAccessStream($file)

    # Load PDF document
    $pdfDoc = Await ([Windows.Data.Pdf.PdfDocument]::LoadFromStreamAsync($randomAccessStream)) ([Windows.Data.Pdf.PdfDocument])

    # Render first page to image
    $page = $pdfDoc.GetPage(0)
    $memStream = New-Object System.IO.MemoryStream
    $outputStream = [System.IO.WindowsRuntimeStreamExtensions]::AsRandomAccessStream($memStream)

    # Render at 300 DPI for good print quality
    $renderOptions = New-Object Windows.Data.Pdf.PdfPageRenderOptions
    $renderOptions.DestinationWidth = [uint32]($page.Size.Width * 4)
    $renderOptions.DestinationHeight = [uint32]($page.Size.Height * 4)

    $null = Await ($page.RenderToStreamAsync($outputStream, $renderOptions)) ([Object])

    $memStream.Position = 0
    $bitmap = [System.Drawing.Image]::FromStream($memStream)

    # Print the image
    $printDoc = New-Object System.Drawing.Printing.PrintDocument
    $printDoc.PrinterSettings.PrinterName = $printerName

    if (-not $printDoc.PrinterSettings.IsValid) {{
        throw "Printer ikke fundet: $printerName"
    }}

    $printDoc.add_PrintPage({{
        param($sender, $e)
        $destRect = $e.MarginBounds
        $e.Graphics.DrawImage($bitmap, $destRect)
    }})

    $printDoc.Print()

    $bitmap.Dispose()
    $memStream.Dispose()
    $file.Close()

    Write-Output "SUCCESS"
}} catch {{
    Write-Error $_.Exception.Message
}}
"#,
            pdf_path_str.replace("'", "''"),
            printer_name.replace("'", "''")
        );

        let output = Command::new("powershell")
            .args(["-ExecutionPolicy", "Bypass", "-Command", &script])
            .output()
            .map_err(|e| format!("PowerShell failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if stdout.trim() == "SUCCESS" {
            return Ok(PrintResult {
                success: true,
                size_kb,
                message: format!("Printed to {}", printer_name),
            });
        } else if !stderr.is_empty() {
            return Err(format!("Print fejl: {}", stderr.trim()));
        } else {
            return Err(format!("Print fejl: Ukendt fejl. stdout={}, stderr={}", stdout.trim(), stderr.trim()));
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
