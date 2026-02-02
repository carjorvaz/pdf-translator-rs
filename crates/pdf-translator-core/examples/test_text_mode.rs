use std::fs;
use std::path::PathBuf;
use lopdf::{Document, Object, Stream};

fn main() {
    let home = std::env::var("HOME").unwrap();
    let pdf_path = PathBuf::from(&home).join("Downloads/fablesdelfontain00lfonrich.pdf");
    let pdf_bytes = fs::read(&pdf_path).expect("Failed to read PDF");
    
    let mut doc = Document::load_mem(&pdf_bytes).expect("Failed to load PDF");
    let pages = doc.get_pages();
    let page_id = *pages.get(&1).expect("No first page");
    
    // Add Helvetica font
    let font_id = doc.add_object(lopdf::Dictionary::from_iter([
        ("Type", Object::Name(b"Font".to_vec())),
        ("Subtype", Object::Name(b"Type1".to_vec())),
        ("BaseFont", Object::Name(b"Helvetica".to_vec())),
    ]));
    
    // Update Resources
    let page_obj = doc.get_object_mut(page_id).expect("Failed to get page");
    if let Object::Dictionary(page_dict) = page_obj {
        if let Ok(Object::Dictionary(res)) = page_dict.get(b"Resources") {
            let mut resources = res.clone();
            if let Ok(Object::Dictionary(fonts)) = resources.get(b"Font") {
                let mut fonts = fonts.clone();
                fonts.set("TestFont", Object::Reference(font_id));
                resources.set("Font", Object::Dictionary(fonts));
            }
            page_dict.set("Resources", Object::Dictionary(resources));
        }
    }
    
    // Create content with EXPLICIT text rendering mode reset (0 Tr = fill text)
    let test_content = b"q
1 1 1 rg
50 550 350 50 re f
0 0 0 rg
BT
0 Tr
/TestFont 24 Tf
60 560 Td
(HELLO WITH TR RESET) Tj
ET
Q
";
    
    let content_stream = Stream::new(lopdf::Dictionary::new(), test_content.to_vec());
    let content_id = doc.add_object(Object::Stream(content_stream));
    
    // Append to contents
    let page_obj = doc.get_object_mut(page_id).expect("Failed to get page");
    if let Object::Dictionary(page_dict) = page_obj {
        if let Ok(Object::Array(mut arr)) = page_dict.get(b"Contents").cloned() {
            arr.push(Object::Reference(content_id));
            page_dict.set("Contents", Object::Array(arr));
        }
    }
    
    let output_path = PathBuf::from(&home).join("Downloads/test_tr_reset.pdf");
    let mut output = Vec::new();
    doc.save_to(&mut output).expect("Failed to save");
    fs::write(&output_path, &output).expect("Failed to write");
    println!("Wrote: {}", output_path.display());
}
