use std::fs;
use std::path::PathBuf;
use pdf_translator_core::pdf::{PdfDocument, TextExtractor};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let (pdf_path, page_num) = if args.len() >= 3 {
        (PathBuf::from(&args[1]), args[2].parse::<usize>().unwrap_or(0))
    } else {
        let home = std::env::var("HOME").unwrap();
        // Default to a test file
        (PathBuf::from(&home).join("Downloads/test.pdf"), 0)
    };

    println!("Reading PDF: {}", pdf_path.display());
    println!("Page: {}\n", page_num);

    let pdf_bytes = fs::read(&pdf_path).expect("Failed to read PDF");
    let doc = PdfDocument::from_bytes(pdf_bytes).expect("Failed to load PDF");

    let extractor = TextExtractor::new(&doc);
    let blocks = extractor.extract_page_blocks(page_num).expect("Failed to extract blocks");

    println!("Found {} text blocks:\n", blocks.len());

    for (i, block) in blocks.iter().enumerate() {
        println!("=== Block {} ===", i);
        println!("BBox: ({:.1}, {:.1}) - ({:.1}, {:.1})",
                 block.bbox.x0, block.bbox.y0, block.bbox.x1, block.bbox.y1);
        println!("Font size: {:.1}pt", block.font_size);
        println!("Lines: {}", block.line_count);
        println!("Text ({} chars): {}", block.text.len(),
                 if block.text.len() > 100 {
                     format!("{}...", &block.text[..100])
                 } else {
                     block.text.clone()
                 });
        println!();
    }
}
