#![allow(
    clippy::expect_used,
    clippy::format_push_string,
    clippy::map_unwrap_or,
    clippy::print_stdout,
    clippy::uninlined_format_args
)]

use skrifa::{charmap::MappingIndex, prelude::FontRef, raw::TableProvider};

// Direct access to the font file for debugging
const NOTO_SERIF: &[u8] = include_bytes!("../assets/NotoSerif-Regular.ttf");

fn main() {
    let face = FontRef::new(NOTO_SERIF).expect("Failed to parse font");
    let mapping = MappingIndex::new(&face);
    let maxp = face.maxp().expect("Failed to read glyph count");
    let hmtx = face.hmtx().expect("Failed to read horizontal metrics");
    let metrics = hmtx.h_metrics();

    let test_text = "HELLO WORLD TEST";
    println!("Testing glyph IDs for: {}", test_text);
    println!("Font has {} glyphs", maxp.num_glyphs());
    println!();

    let mut hex_string = String::new();
    for c in test_text.chars() {
        let gid = mapping
            .charmap(&face)
            .map(c)
            .map_or(0, skrifa::GlyphId::to_u32);
        let width = usize::try_from(gid)
            .ok()
            .and_then(|glyph_index| metrics.get(glyph_index).or_else(|| metrics.last()))
            .map_or(0, |metric| metric.advance.get());
        println!(
            "'{}' (U+{:04X}) -> GID {:4} (0x{:04X}) width: {}",
            c, c as u32, gid, gid, width
        );
        hex_string.push_str(&format!("{:04X}", gid));
    }

    println!();
    println!("Hex string: <{}>", hex_string);
}
