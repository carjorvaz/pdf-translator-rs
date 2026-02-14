//! TrueType font embedding for PDF overlay text.
//!
//! This module provides proper Unicode support by embedding Noto Serif
//! as a CIDFont with Identity-H encoding. This allows any Unicode character
//! (that the font supports) to be rendered correctly in the PDF.
//!
//! # PDF Font Structure
//!
//! For Unicode text, PDFs use a composite font structure:
//! - **Type0 font**: The top-level font dictionary that references:
//!   - **CIDFont**: Contains glyph metrics and references:
//!     - **FontDescriptor**: Font metadata (flags, bounding box, etc.)
//!     - **FontFile2**: The embedded TrueType font program
//!   - **ToUnicode CMap**: Maps glyph IDs back to Unicode for copy/paste

use std::sync::LazyLock;

use lopdf::{Document, Object, ObjectId, Stream};
use ttf_parser::Face;

use crate::error::{Error, Result};

/// Noto Serif Regular font, embedded at compile time.
/// This is Google's open-source font (OFL license) designed for readability
/// with excellent Unicode coverage across Latin, Cyrillic, and Greek scripts.
const NOTO_SERIF: &[u8] = include_bytes!("../../assets/NotoSerif-Regular.ttf");

/// Global font instance, parsed once on first use.
static GLOBAL_FONT: LazyLock<EmbeddedFont> = LazyLock::new(|| {
    EmbeddedFont::new().expect("Failed to parse embedded Noto Serif font")
});

/// Handles TrueType font embedding in PDFs.
pub struct EmbeddedFont {
    face: Face<'static>,
}

impl EmbeddedFont {
    /// Create a new embedded font handler.
    fn new() -> Result<Self> {
        let face = Face::parse(NOTO_SERIF, 0)
            .map_err(|e| Error::PdfOverlay(format!("Failed to parse font: {e}")))?;
        Ok(Self { face })
    }

    /// Get the global shared font instance.
    pub fn global() -> &'static Self {
        &GLOBAL_FONT
    }

    /// Get the glyph ID for a character, falling back to .notdef (0) if not found.
    pub fn glyph_id(&self, c: char) -> u16 {
        self.face.glyph_index(c).map_or(0, |g| g.0)
    }

    /// Get the advance width of a glyph in font units.
    pub fn glyph_width(&self, glyph_id: u16) -> u16 {
        self.face
            .glyph_hor_advance(ttf_parser::GlyphId(glyph_id))
            .unwrap_or(0)
    }

    /// Get the font's units per em.
    pub fn units_per_em(&self) -> u16 {
        self.face.units_per_em()
    }

    /// Calculate the width of a string in PDF points at the given font size.
    #[allow(clippy::cast_precision_loss)] // Precision loss acceptable for width calculations
    pub fn string_width(&self, text: &str, font_size: f32) -> f32 {
        let units_per_em = f32::from(self.units_per_em());
        let total_units: u32 = text
            .chars()
            .map(|c| u32::from(self.glyph_width(self.glyph_id(c))))
            .sum();
        total_units as f32 * font_size / units_per_em
    }

    /// Convert text to a hex string of glyph IDs for PDF content streams.
    /// Returns the hex string without angle brackets.
    pub fn text_to_hex_glyphs(&self, text: &str) -> String {
        use std::fmt::Write;
        text.chars().fold(String::new(), |mut acc, c| {
            let _ = write!(acc, "{:04X}", self.glyph_id(c));
            acc
        })
    }

    /// Embed this font into a PDF document and add it to a page's resources.
    /// Returns the font name to use in content streams (e.g., "/FTrans").
    pub fn embed_in_document(
        &self,
        doc: &mut Document,
        page_id: ObjectId,
    ) -> Result<&'static str> {
        // Create all the font objects
        let font_file_id = self.create_font_file(doc);
        let font_descriptor_id = self.create_font_descriptor(doc, font_file_id);
        let cid_font_id = self.create_cid_font(doc, font_descriptor_id);
        let to_unicode_id = self.create_to_unicode_cmap(doc);
        let type0_font_id = self.create_type0_font(doc, cid_font_id, to_unicode_id);

        // Add the font to the page's resources
        self.add_font_to_page(doc, page_id, type0_font_id)?;

        Ok("FTrans")
    }

    /// Create the FontFile2 stream containing the raw TrueType data.
    #[allow(clippy::unused_self)] // Kept as method for API consistency
    #[allow(clippy::cast_possible_wrap)] // Font size always fits in i64
    fn create_font_file(&self, doc: &mut Document) -> ObjectId {
        let mut dict = lopdf::Dictionary::new();
        dict.set("Length1", Object::Integer(NOTO_SERIF.len() as i64));

        let stream = Stream::new(dict, NOTO_SERIF.to_vec()).with_compression(true);
        doc.add_object(Object::Stream(stream))
    }

    /// Create the FontDescriptor dictionary with font metrics.
    fn create_font_descriptor(&self, doc: &mut Document, font_file_id: ObjectId) -> ObjectId {
        let bbox = self.face.global_bounding_box();

        let dict = lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"FontDescriptor".to_vec())),
            ("FontName", Object::Name(b"NotoSerif".to_vec())),
            ("FontFamily", Object::String(b"Noto Serif".to_vec(), lopdf::StringFormat::Literal)),
            ("Flags", Object::Integer(32)), // Nonsymbolic
            ("FontBBox", Object::Array(vec![
                Object::Integer(i64::from(bbox.x_min)),
                Object::Integer(i64::from(bbox.y_min)),
                Object::Integer(i64::from(bbox.x_max)),
                Object::Integer(i64::from(bbox.y_max)),
            ])),
            ("ItalicAngle", Object::Integer(0)),
            ("Ascent", Object::Integer(i64::from(self.face.ascender()))),
            ("Descent", Object::Integer(i64::from(self.face.descender()))),
            ("CapHeight", Object::Integer(i64::from(self.face.capital_height().unwrap_or_else(|| self.face.ascender())))),
            ("StemV", Object::Integer(90)), // Approximate value for serif
            ("FontFile2", Object::Reference(font_file_id)),
        ]);

        doc.add_object(Object::Dictionary(dict))
    }

    /// Create the CIDFont dictionary with per-glyph width information.
    fn create_cid_font(&self, doc: &mut Document, font_descriptor_id: ObjectId) -> ObjectId {
        // Build the W (widths) array for proper character spacing
        // Format: [gid [w1 w2 ...]] for consecutive glyphs starting at gid
        let widths_array = self.build_widths_array();

        // Default width for any glyph not in the W array (use space width, scaled)
        let default_width = self.scale_width(self.glyph_width(self.glyph_id(' ')));

        let dict = lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"CIDFontType2".to_vec())),
            ("BaseFont", Object::Name(b"NotoSerif".to_vec())),
            ("CIDSystemInfo", Object::Dictionary(lopdf::Dictionary::from_iter([
                ("Registry", Object::String(b"Adobe".to_vec(), lopdf::StringFormat::Literal)),
                ("Ordering", Object::String(b"Identity".to_vec(), lopdf::StringFormat::Literal)),
                ("Supplement", Object::Integer(0)),
            ]))),
            ("FontDescriptor", Object::Reference(font_descriptor_id)),
            ("DW", Object::Integer(default_width)),
            ("W", Object::Array(widths_array)),
            ("CIDToGIDMap", Object::Name(b"Identity".to_vec())),
        ]);

        doc.add_object(Object::Dictionary(dict))
    }

    /// Scale a font-unit width to PDF's 1000-unit system.
    fn scale_width(&self, width: u16) -> i64 {
        // PDF expects widths in 1/1000ths of text space
        // TrueType widths are in font design units (e.g., 2048 per em)
        let units_per_em = i64::from(self.face.units_per_em());
        (i64::from(width) * 1000) / units_per_em
    }

    /// Build the W (widths) array for CIDFont.
    /// The W array format is: [gid [w1 w2 ...]] for consecutive GIDs starting at gid.
    fn build_widths_array(&self) -> Vec<Object> {
        use std::collections::BTreeMap;

        // Collect (GID -> scaled_width) for all characters we care about
        let mut gid_widths: BTreeMap<u16, i64> = BTreeMap::new();

        // Define character ranges to include widths for
        let ranges: &[(u32, u32)] = &[
            (0x0020, 0x007F), // Basic Latin (ASCII printable)
            (0x00A0, 0x00FF), // Latin-1 Supplement
            (0x0100, 0x017F), // Latin Extended-A
            (0x0180, 0x024F), // Latin Extended-B
            (0x2000, 0x206F), // General Punctuation (smart quotes, dashes, etc.)
            (0x20AC, 0x20AC), // Euro sign
        ];

        for &(start, end) in ranges {
            for codepoint in start..=end {
                if let Some(c) = char::from_u32(codepoint) {
                    let gid = self.glyph_id(c);
                    if gid != 0 {
                        let width = self.glyph_width(gid);
                        let scaled = self.scale_width(width);
                        gid_widths.insert(gid, scaled);
                    }
                }
            }
        }

        // Build W array from sorted GIDs, grouping consecutive runs
        let mut result = Vec::new();
        let mut iter = gid_widths.iter().peekable();

        while let Some((&first_gid, &first_width)) = iter.next() {
            let mut widths = vec![Object::Integer(first_width)];
            let mut expected_next = first_gid + 1;

            // Collect consecutive GIDs
            while let Some(&(&gid, &width)) = iter.peek() {
                if gid == expected_next {
                    widths.push(Object::Integer(width));
                    expected_next += 1;
                    iter.next();
                } else {
                    break;
                }
            }

            result.push(Object::Integer(i64::from(first_gid)));
            result.push(Object::Array(widths));
        }

        result
    }

    /// Create a ToUnicode CMap for text extraction/copy-paste support.
    #[allow(clippy::unused_self)] // Kept as method for API consistency
    fn create_to_unicode_cmap(&self, doc: &mut Document) -> ObjectId {
        // This is a simplified Identity CMap that maps glyph IDs directly to Unicode
        // For a complete implementation, you'd generate mappings for all used glyphs
        let cmap = b"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo <<
  /Registry (Adobe)
  /Ordering (UCS)
  /Supplement 0
>> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfrange
<0000> <FFFF> <0000>
endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end";

        let stream = Stream::new(lopdf::Dictionary::new(), cmap.to_vec());
        doc.add_object(Object::Stream(stream))
    }

    /// Create the Type0 (composite) font dictionary.
    #[allow(clippy::unused_self)] // Kept as method for API consistency
    fn create_type0_font(
        &self,
        doc: &mut Document,
        cid_font_id: ObjectId,
        to_unicode_id: ObjectId,
    ) -> ObjectId {
        let dict = lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type0".to_vec())),
            ("BaseFont", Object::Name(b"NotoSerif".to_vec())),
            ("Encoding", Object::Name(b"Identity-H".to_vec())),
            ("DescendantFonts", Object::Array(vec![Object::Reference(cid_font_id)])),
            ("ToUnicode", Object::Reference(to_unicode_id)),
        ]);

        doc.add_object(Object::Dictionary(dict))
    }

    /// Add the font to a page's Resources dictionary.
    ///
    /// Handles both inline Resources dictionaries and indirect references,
    /// which are common in complex PDFs (e.g., from archive.org).
    #[allow(clippy::unused_self)] // Kept as method for API consistency
    fn add_font_to_page(
        &self,
        doc: &mut Document,
        page_id: ObjectId,
        font_id: ObjectId,
    ) -> Result<()> {
        // First, resolve the Resources dictionary (may be inline or a reference)
        let mut resources = self.resolve_resources(doc, page_id)?;

        // Resolve the Font dictionary within Resources (also may be a reference)
        let mut fonts = if let Ok(font_obj) = resources.get(b"Font") {
            match font_obj {
                Object::Dictionary(d) => d.clone(),
                Object::Reference(ref_id) => {
                    if let Ok(Object::Dictionary(d)) = doc.get_object(*ref_id) {
                        d.clone()
                    } else {
                        lopdf::Dictionary::new()
                    }
                }
                _ => lopdf::Dictionary::new(),
            }
        } else {
            lopdf::Dictionary::new()
        };

        // Add our font as FTrans
        fonts.set("FTrans", Object::Reference(font_id));
        resources.set("Font", Object::Dictionary(fonts));

        // Set the updated Resources back on the page (as inline dict to capture our changes)
        let page = doc.get_object_mut(page_id)
            .map_err(|e| Error::Lopdf(format!("Failed to get page: {e}")))?;

        if let Object::Dictionary(page_dict) = page {
            page_dict.set("Resources", Object::Dictionary(resources));
        }

        Ok(())
    }

    /// Resolve the Resources dictionary for a page, handling indirect references
    /// and inheritance from parent Pages nodes.
    ///
    /// PDF pages can have Resources as:
    /// - An inline dictionary: `/Resources << /Font << ... >> >>`
    /// - An indirect reference: `/Resources 5 0 R`
    /// - Inherited from parent Pages node (common in complex PDFs like archive.org)
    fn resolve_resources(&self, doc: &Document, page_id: ObjectId) -> Result<lopdf::Dictionary> {
        let page = doc.get_object(page_id)
            .map_err(|e| Error::Lopdf(format!("Failed to get page: {e}")))?;

        if let Object::Dictionary(page_dict) = page {
            // First, try to get Resources directly from the page
            if let Ok(res_obj) = page_dict.get(b"Resources") {
                if let Some(dict) = self.resolve_dict_object(doc, res_obj) {
                    return Ok(dict);
                }
            }

            // If not found, try to inherit from parent Pages node
            if let Ok(parent_obj) = page_dict.get(b"Parent") {
                if let Some(dict) = self.resolve_inherited_resources(doc, parent_obj) {
                    return Ok(dict);
                }
            }
        }

        // No Resources found - create empty dictionary
        Ok(lopdf::Dictionary::new())
    }

    /// Resolve an object that should be a Dictionary (handles References).
    fn resolve_dict_object(&self, doc: &Document, obj: &Object) -> Option<lopdf::Dictionary> {
        match obj {
            Object::Dictionary(d) => Some(d.clone()),
            Object::Reference(ref_id) => {
                if let Ok(Object::Dictionary(d)) = doc.get_object(*ref_id) {
                    Some(d.clone())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Walk up the Pages tree to find inherited Resources.
    ///
    /// Uses a depth limit to prevent infinite recursion on malformed PDFs
    /// with circular Parent references.
    fn resolve_inherited_resources(&self, doc: &Document, parent_obj: &Object) -> Option<lopdf::Dictionary> {
        self.resolve_inherited_resources_recursive(doc, parent_obj, 10)
    }

    fn resolve_inherited_resources_recursive(
        &self,
        doc: &Document,
        parent_obj: &Object,
        depth: usize,
    ) -> Option<lopdf::Dictionary> {
        if depth == 0 {
            return None;
        }

        let parent_id = match parent_obj {
            Object::Reference(id) => *id,
            _ => return None,
        };

        let parent = match doc.get_object(parent_id) {
            Ok(Object::Dictionary(d)) => d,
            _ => return None,
        };

        // Check if this parent node has Resources
        if let Ok(res_obj) = parent.get(b"Resources") {
            if let Some(dict) = self.resolve_dict_object(doc, res_obj) {
                return Some(dict);
            }
        }

        // Continue up the tree
        if let Ok(grandparent_obj) = parent.get(b"Parent") {
            return self.resolve_inherited_resources_recursive(doc, grandparent_obj, depth - 1);
        }

        None
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_loads() {
        let font = EmbeddedFont::global();
        assert!(font.units_per_em() > 0);
    }

    #[test]
    fn test_glyph_lookup() {
        let font = EmbeddedFont::global();
        // Space and 'A' should have valid glyph IDs
        assert!(font.glyph_id(' ') > 0 || font.glyph_id('A') > 0);
    }

    #[test]
    fn test_hex_conversion() {
        let font = EmbeddedFont::global();
        let hex = font.text_to_hex_glyphs("A");
        // Should be 4 hex digits for one character
        assert_eq!(hex.len(), 4);
    }
}
