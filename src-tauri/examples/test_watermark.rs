// Standalone test binary: read a PDF, watermark it, write to disk.
// Run with: cargo run --example test_watermark -- /path/to/in.pdf /path/to/out.pdf

use std::env;
use std::fs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <in.pdf> <out.pdf>", args[0]);
        std::process::exit(2);
    }
    let input = &args[1];
    let output = &args[2];

    let pdf_bytes = fs::read(input)?;
    println!("Loaded {} ({} bytes)", input, pdf_bytes.len());

    let watermarked = app_lib::add_logo_watermark_for_test(pdf_bytes);
    println!("Watermarked size: {} bytes", watermarked.len());

    fs::write(output, &watermarked)?;
    println!("Wrote {}", output);
    Ok(())
}
