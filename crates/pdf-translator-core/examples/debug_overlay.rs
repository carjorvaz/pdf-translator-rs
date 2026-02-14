use std::fs;
use std::path::PathBuf;
use pdf_translator_core::pdf::{PdfOverlay, OverlayOptions, TranslationOverlay, BoundingBox};

fn main() {
    let home = std::env::var("HOME").unwrap();
    let pdf_path = PathBuf::from(&home).join("Downloads/fablesdelfontain00lfonrich.pdf");
    let pdf_bytes = fs::read(&pdf_path).expect("Failed to read PDF");
    
    println!("Input PDF size: {} bytes", pdf_bytes.len());
    
    // Create overlay with test text
    let overlay = PdfOverlay::new(OverlayOptions::default());
    
    let test_overlays = vec![
        TranslationOverlay {
            bbox: BoundingBox { x0: 50.0, y0: 50.0, x1: 400.0, y1: 150.0 },
            original: "Test original".to_string(),
            translated: "HELLO WORLD TEST".to_string(),
            font_size: 24.0,
        }
    ];
    
    let result = overlay.apply_overlays(&pdf_bytes, 0, &test_overlays)
        .expect("Failed to apply overlays");
    
    let output_path = PathBuf::from(&home).join("Downloads/test_overlay_output.pdf");
    fs::write(&output_path, &result).expect("Failed to write output");
    println!("Wrote output to: {}", output_path.display());
    println!("Output PDF size: {} bytes", result.len());
    
    // Now examine the output
    let doc = lopdf::Document::load_mem(&result).expect("Failed to load output PDF");
    let pages = doc.get_pages();
    let first_page_id = pages.get(&1).expect("No first page");
    let page_obj = doc.get_object(*first_page_id).expect("Failed to get page object");
    
    if let lopdf::Object::Dictionary(page_dict) = page_obj {
        println!("\nOutput PDF page structure:");
        
        // Check Resources
        if let Ok(res) = page_dict.get(b"Resources") {
            if let lopdf::Object::Dictionary(res_dict) = res {
                println!("Resources entries: {}", res_dict.len());
                if let Ok(font) = res_dict.get(b"Font") {
                    if let lopdf::Object::Dictionary(font_dict) = font {
                        println!("Font entries:");
                        for (key, val) in font_dict.iter() {
                            println!("  {} => {:?}", String::from_utf8_lossy(key), val);
                        }
                    }
                }
            }
        }
        
        // Check Contents
        if let Ok(contents) = page_dict.get(b"Contents") {
            match contents {
                lopdf::Object::Array(arr) => {
                    println!("\nContents array has {} entries", arr.len());
                    // Show last content stream (our overlay)
                    if let Some(lopdf::Object::Reference(last_ref)) = arr.last() {
                        if let Ok(lopdf::Object::Stream(stream)) = doc.get_object(*last_ref) {
                            let content = stream.decompressed_content().unwrap_or_else(|_| stream.content.clone());
                            let text = String::from_utf8_lossy(&content);
                            println!("\nLast content stream (our overlay):");
                            println!("{}", &text[..text.len().min(1000)]);
                        }
                    }
                }
                _ => println!("Contents is not an array"),
            }
        }
    }
}
