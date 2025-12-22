use std::process::Command;
use tauri::Manager;
use base64::Engine;
use pdfium_render::prelude::*;

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

    // Print using system command (macOS and Linux use lp, Windows uses native API)
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
        // Use native Windows printing with pdfium
        print_pdf_windows(&pdf_bytes, &printer_name, &job_name, size_kb)
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

#[cfg(target_os = "windows")]
fn print_pdf_windows(pdf_bytes: &[u8], printer_name: &str, job_name: &str, size_kb: usize) -> Result<PrintResult, String> {
    use windows::core::{PCSTR, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Graphics::Printing::*;

    // Try to load pdfium - first from app directory, then system
    let pdfium = Pdfium::new(
        Pdfium::bind_to_library(
            Pdfium::pdfium_platform_library_name_at_path("./")
        )
        .or_else(|_| Pdfium::bind_to_system_library())
        .map_err(|e| format!("Failed to load pdfium: {}. Please ensure pdfium.dll is in the app directory.", e))?
    );

    // Load PDF from bytes
    let document = pdfium.load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| format!("Failed to load PDF: {}", e))?;

    let page_count = document.pages().len();
    if page_count == 0 {
        return Err("PDF has no pages".to_string());
    }

    // Render first page to image (for label printing, usually only 1 page)
    let page = document.pages().get(0)
        .map_err(|e| format!("Failed to get page: {}", e))?;

    // Render at 300 DPI for good print quality
    // Label printers typically expect ~203 DPI, but higher is fine
    let render_config = PdfRenderConfig::new()
        .set_target_width(1200)  // ~4 inches at 300 DPI
        .set_maximum_height(1800); // ~6 inches at 300 DPI

    let bitmap = page.render_with_config(&render_config)
        .map_err(|e| format!("Failed to render PDF: {}", e))?;

    let image = bitmap.as_image();
    let rgb_image = image.to_rgb8();
    let width = rgb_image.width();
    let height = rgb_image.height();

    // Convert printer name to wide string
    let printer_wide: Vec<u16> = printer_name.encode_utf16().chain(std::iter::once(0)).collect();
    let job_wide: Vec<u16> = job_name.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        // Create printer DC
        let hdc = CreateDCW(
            PCWSTR::null(),
            PCWSTR(printer_wide.as_ptr()),
            PCWSTR::null(),
            None
        );

        if hdc.is_invalid() {
            return Err(format!("Failed to create printer DC for '{}'", printer_name));
        }

        // Start document
        let doc_info = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(job_wide.as_ptr()),
            lpszOutput: PCWSTR::null(),
            lpszDatatype: PCWSTR::null(),
            fwType: 0,
        };

        let doc_result = StartDocW(hdc, &doc_info);
        if doc_result <= 0 {
            DeleteDC(hdc);
            return Err("Failed to start print document".to_string());
        }

        // Start page
        if StartPage(hdc) <= 0 {
            EndDoc(hdc);
            DeleteDC(hdc);
            return Err("Failed to start print page".to_string());
        }

        // Get printer page size
        let page_width = GetDeviceCaps(hdc, HORZRES);
        let page_height = GetDeviceCaps(hdc, VERTRES);

        // Calculate scaling to fit page while maintaining aspect ratio
        let img_ratio = width as f64 / height as f64;
        let page_ratio = page_width as f64 / page_height as f64;

        let (dest_width, dest_height) = if img_ratio > page_ratio {
            // Image is wider - fit to width
            (page_width, (page_width as f64 / img_ratio) as i32)
        } else {
            // Image is taller - fit to height
            ((page_height as f64 * img_ratio) as i32, page_height)
        };

        // Create bitmap info header
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // Negative for top-down
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD::default()],
        };

        // Convert RGB to BGR (Windows bitmap format)
        let mut bgr_data: Vec<u8> = Vec::with_capacity((width * height * 3) as usize);
        for pixel in rgb_image.pixels() {
            bgr_data.push(pixel[2]); // B
            bgr_data.push(pixel[1]); // G
            bgr_data.push(pixel[0]); // R
        }

        // Pad rows to 4-byte boundary
        let row_size = ((width * 3 + 3) / 4) * 4;
        let mut padded_data: Vec<u8> = Vec::with_capacity((row_size * height) as usize);
        for y in 0..height {
            let row_start = (y * width * 3) as usize;
            let row_end = row_start + (width * 3) as usize;
            padded_data.extend_from_slice(&bgr_data[row_start..row_end]);
            // Add padding
            for _ in 0..(row_size - width * 3) {
                padded_data.push(0);
            }
        }

        // Draw the image
        let result = StretchDIBits(
            hdc,
            0, 0, dest_width, dest_height,  // Destination
            0, 0, width as i32, height as i32,  // Source
            Some(padded_data.as_ptr() as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );

        if result == 0 {
            EndPage(hdc);
            EndDoc(hdc);
            DeleteDC(hdc);
            return Err("Failed to draw image to printer".to_string());
        }

        // End page and document
        EndPage(hdc);
        EndDoc(hdc);
        DeleteDC(hdc);
    }

    Ok(PrintResult {
        success: true,
        size_kb,
        message: format!("Printed {}x{} image to {}", width, height, printer_name),
    })
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
