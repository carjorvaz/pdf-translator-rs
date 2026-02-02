// Direct access to the font file for debugging
const NOTO_SERIF: &[u8] = include_bytes!("../assets/NotoSerif-Regular.ttf");

fn main() {
    let face = ttf_parser::Face::parse(NOTO_SERIF, 0).expect("Failed to parse font");
    
    let test_text = "HELLO WORLD TEST";
    println!("Testing glyph IDs for: {}", test_text);
    println!("Font has {} glyphs", face.number_of_glyphs());
    println!();
    
    let mut hex_string = String::new();
    for c in test_text.chars() {
        let gid = face.glyph_index(c).map(|g| g.0).unwrap_or(0);
        let width = face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap_or(0);
        println!("'{}' (U+{:04X}) -> GID {:4} (0x{:04X}) width: {}", c, c as u32, gid, gid, width);
        hex_string.push_str(&format!("{:04X}", gid));
    }
    
    println!();
    println!("Hex string: <{}>", hex_string);
}
