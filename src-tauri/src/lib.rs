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

        // Create log file path for debugging
        let log_path = temp_dir.path().join("print_debug.log");
        let log_path_str = log_path.display().to_string();

        // Comprehensive Windows print script with detailed logging
        let script = format!(
            r#"
$ErrorActionPreference = "Stop"
$logFile = '{log_path}'

function Log($msg) {{
    $timestamp = Get-Date -Format "HH:mm:ss.fff"
    "$timestamp - $msg" | Add-Content -Path $logFile -Encoding UTF8
    Write-Host $msg
}}

try {{
    $pdfPath = '{pdf_path}'
    $printerName = '{printer_name}'

    Log "=== GLS Print Agent Debug Log ==="
    Log "PDF Path: $pdfPath"
    Log "Printer: $printerName"
    Log "Windows Version: $([System.Environment]::OSVersion.Version)"

    # Check if PDF exists
    if (-not (Test-Path $pdfPath)) {{
        throw "PDF fil ikke fundet: $pdfPath"
    }}
    Log "PDF fil fundet: $((Get-Item $pdfPath).Length) bytes"

    # Check printer exists
    $printer = Get-Printer -Name $printerName -ErrorAction SilentlyContinue
    if (-not $printer) {{
        Log "Tilgaengelige printere:"
        Get-Printer | ForEach-Object {{ Log "  - $($_.Name)" }}
        throw "Printer ikke fundet: $printerName"
    }}
    Log "Printer fundet: $($printer.Name), Driver: $($printer.DriverName), Port: $($printer.PortName)"

    # Method 1: Try using .NET System.Drawing.Printing with PDF rendered via WinRT
    Log "Forsøger Windows.Data.Pdf rendering..."

    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Runtime.WindowsRuntime

    # Load WinRT types
    $null = [Windows.Storage.StorageFile, Windows.Storage, ContentType=WindowsRuntime]
    $null = [Windows.Data.Pdf.PdfDocument, Windows.Data.Pdf, ContentType=WindowsRuntime]

    # Helper for async
    Add-Type -TypeDefinition @"
using System;
using System.Threading.Tasks;
using Windows.Data.Pdf;
using Windows.Storage;
using Windows.Storage.Streams;

public static class PdfHelper {{
    public static PdfDocument LoadPdf(string path) {{
        var file = StorageFile.GetFileFromPathAsync(path).AsTask().Result;
        return PdfDocument.LoadFromFileAsync(file).AsTask().Result;
    }}

    public static byte[] RenderPage(PdfPage page, uint width, uint height) {{
        using (var stream = new InMemoryRandomAccessStream()) {{
            var options = new PdfPageRenderOptions();
            options.DestinationWidth = width;
            options.DestinationHeight = height;
            page.RenderToStreamAsync(stream, options).AsTask().Wait();

            stream.Seek(0);
            var bytes = new byte[stream.Size];
            var reader = new DataReader(stream);
            reader.LoadAsync((uint)stream.Size).AsTask().Wait();
            reader.ReadBytes(bytes);
            return bytes;
        }}
    }}
}}
"@ -ReferencedAssemblies @(
    "System.Runtime.WindowsRuntime",
    "$([System.Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory())System.Runtime.dll",
    "$env:SystemRoot\System32\WinMetadata\Windows.Foundation.winmd",
    "$env:SystemRoot\System32\WinMetadata\Windows.Storage.winmd",
    "$env:SystemRoot\System32\WinMetadata\Windows.Data.winmd"
) -ErrorAction Stop

    Log "WinRT types loaded successfully"

    # Load and render PDF
    $pdfDoc = [PdfHelper]::LoadPdf($pdfPath)
    Log "PDF loaded: $($pdfDoc.PageCount) pages"

    $page = $pdfDoc.GetPage(0)
    $pageWidth = [uint32]($page.Size.Width * 3)
    $pageHeight = [uint32]($page.Size.Height * 3)
    Log "Page size: $($page.Size.Width) x $($page.Size.Height), rendering at: $pageWidth x $pageHeight"

    $imageBytes = [PdfHelper]::RenderPage($page, $pageWidth, $pageHeight)
    Log "Rendered to $($imageBytes.Length) bytes"

    # Convert to bitmap
    $memStream = New-Object System.IO.MemoryStream(,$imageBytes)
    $bitmap = [System.Drawing.Image]::FromStream($memStream)
    Log "Bitmap created: $($bitmap.Width) x $($bitmap.Height)"

    # Print
    $printDoc = New-Object System.Drawing.Printing.PrintDocument
    $printDoc.PrinterSettings.PrinterName = $printerName
    $printDoc.DocumentName = "GLS Label"

    # Use StandardPrintController for silent printing (no dialog)
    $printDoc.PrintController = New-Object System.Drawing.Printing.StandardPrintController

    Log "Printer valid: $($printDoc.PrinterSettings.IsValid)"
    Log "Paper size: $($printDoc.DefaultPageSettings.PaperSize.PaperName)"

    $script:printBitmap = $bitmap
    $printDoc.add_PrintPage({{
        param($sender, $e)

        # Scale image to fit the printable area while maintaining aspect ratio
        $imgRatio = $script:printBitmap.Width / $script:printBitmap.Height
        $pageRatio = $e.MarginBounds.Width / $e.MarginBounds.Height

        $destWidth = $e.MarginBounds.Width
        $destHeight = $e.MarginBounds.Height

        if ($imgRatio -gt $pageRatio) {{
            $destHeight = $destWidth / $imgRatio
        }} else {{
            $destWidth = $destHeight * $imgRatio
        }}

        $destRect = New-Object System.Drawing.RectangleF(
            $e.MarginBounds.X,
            $e.MarginBounds.Y,
            $destWidth,
            $destHeight
        )

        $e.Graphics.DrawImage($script:printBitmap, $destRect)
    }})

    Log "Starting print..."
    $printDoc.Print()
    Log "Print command sent successfully"

    $bitmap.Dispose()
    $memStream.Dispose()
    $page.Dispose()
    $pdfDoc.Dispose()

    Log "=== PRINT SUCCESSFUL ==="
    Write-Output "SUCCESS"

}} catch {{
    $errorMsg = $_.Exception.Message
    if ($_.Exception.InnerException) {{
        $errorMsg += " Inner: " + $_.Exception.InnerException.Message
    }}
    Log "ERROR: $errorMsg"
    Log "Stack: $($_.ScriptStackTrace)"
    Write-Error $errorMsg
}}
"#,
            log_path = log_path_str.replace("\\", "\\\\").replace("'", "''"),
            pdf_path = pdf_path_str.replace("\\", "\\\\").replace("'", "''"),
            printer_name = printer_name.replace("'", "''")
        );

        let output = Command::new("powershell")
            .args(["-ExecutionPolicy", "Bypass", "-NoProfile", "-Command", &script])
            .output()
            .map_err(|e| format!("PowerShell failed to start: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Read the debug log if it exists
        let debug_log = std::fs::read_to_string(&log_path).unwrap_or_default();

        if stdout.trim().contains("SUCCESS") {
            return Ok(PrintResult {
                success: true,
                size_kb,
                message: format!("Printed to {}", printer_name),
            });
        } else {
            // Return detailed error with log contents
            let error_detail = if !stderr.is_empty() {
                format!("{}\n\nLog:\n{}", stderr.trim(), debug_log)
            } else if !debug_log.is_empty() {
                format!("Print failed.\n\nLog:\n{}", debug_log)
            } else {
                format!("Unknown error. stdout={}, stderr={}", stdout.trim(), stderr.trim())
            };
            return Err(error_detail);
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
