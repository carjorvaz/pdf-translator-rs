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

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;
use std::sync::LazyLock;

use lopdf::{Document, Object, ObjectId, Stream};
use skrifa::{charmap::MappingIndex, prelude::FontRef, raw::TableProvider};

use crate::error::{Error, Result};

/// Noto Serif Regular font, embedded at compile time.
/// This is Google's open-source font (OFL license) designed for readability
/// with excellent Unicode coverage across Latin, Cyrillic, and Greek scripts.
const NOTO_SERIF: &[u8] = include_bytes!("../../assets/NotoSerif-Regular.ttf");

/// Global font instance, parsed once on first use.
#[allow(clippy::expect_used)]
static GLOBAL_FONT: LazyLock<EmbeddedFont> =
    LazyLock::new(|| EmbeddedFont::new().expect("Failed to parse embedded Noto Serif font"));

/// Handles TrueType font embedding in PDFs.
pub struct EmbeddedFont {
    face: FontRef<'static>,
    mapping_index: MappingIndex,
    units_per_em: u16,
    bounding_box: [i16; 4],
    ascender: i16,
    descender: i16,
    capital_height: i16,
}
#[derive(Debug)]
struct EncodedGlyph {
    cid: u16,
    gid: u16,
    character: char,
}

/// Per-document mapping from emitted Unicode scalars to PDF CIDs and TrueType GIDs.
pub(super) struct FontEncoding {
    by_character: BTreeMap<char, u16>,
    glyphs: Vec<EncodedGlyph>,
}

impl EmbeddedFont {
    /// Create a new embedded font handler.
    fn new() -> Result<Self> {
        let face = FontRef::new(NOTO_SERIF)
            .map_err(|error| Error::PdfOverlay(format!("Failed to parse font: {error}")))?;
        let mapping_index = MappingIndex::new(&face);
        let head = face
            .head()
            .map_err(|error| Error::PdfOverlay(format!("Failed to read font metrics: {error}")))?;
        let units_per_em = head.units_per_em();
        let bounding_box = [head.x_min(), head.y_min(), head.x_max(), head.y_max()];
        let hhea = face
            .hhea()
            .map_err(|error| Error::PdfOverlay(format!("Failed to read font metrics: {error}")))?;
        let ascender = hhea.ascender().to_i16();
        let descender = hhea.descender().to_i16();
        let capital_height = face
            .os2()
            .ok()
            .and_then(|os2| os2.s_cap_height())
            .unwrap_or(ascender);
        Ok(Self {
            face,
            mapping_index,
            units_per_em,
            bounding_box,
            ascender,
            descender,
            capital_height,
        })
    }

    /// Get the global shared font instance.
    pub fn global() -> &'static Self {
        &GLOBAL_FONT
    }

    /// Get the glyph ID for a character.
    ///
    /// Unsupported scalars use the visible replacement-character glyph when
    /// available, otherwise the font's `.notdef` glyph.
    pub fn glyph_id(&self, character: char) -> u16 {
        self.mapping_index
            .charmap(&self.face)
            .map(character)
            .and_then(|glyph| u16::try_from(glyph.to_u32()).ok())
            .unwrap_or_else(|| self.fallback_glyph_id())
    }

    fn fallback_glyph_id(&self) -> u16 {
        self.mapping_index
            .charmap(&self.face)
            .map('\u{FFFD}')
            .and_then(|glyph| u16::try_from(glyph.to_u32()).ok())
            .unwrap_or(0)
    }

    /// Get the advance width of a glyph in font units.
    pub fn glyph_width(&self, glyph_id: u16) -> u16 {
        let Ok(maxp) = self.face.maxp() else {
            return 0;
        };
        if u32::from(glyph_id) >= u32::from(maxp.num_glyphs()) {
            return 0;
        }

        self.face
            .hmtx()
            .ok()
            .and_then(|hmtx| {
                let metrics = hmtx.h_metrics();
                metrics
                    .get(usize::from(glyph_id))
                    .or_else(|| metrics.last())
                    .map(|metric| metric.advance.get())
            })
            .unwrap_or(0)
    }

    /// Get the font's units per em.
    pub const fn units_per_em(&self) -> u16 {
        self.units_per_em
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

    /// Build a stable CID assignment for the characters emitted by a content stream.
    pub(super) fn encoding_for_characters<I>(&self, characters: I) -> Result<FontEncoding>
    where
        I: IntoIterator<Item = char>,
    {
        let characters: BTreeSet<char> = characters.into_iter().collect();
        if characters.len() > usize::from(u16::MAX) {
            return Err(Error::PdfOverlay(format!(
                "Overlay uses {} distinct characters; PDF CID fonts support at most {}",
                characters.len(),
                u16::MAX
            )));
        }

        let mut by_character = BTreeMap::new();
        let mut glyphs = Vec::with_capacity(characters.len());
        for (index, character) in characters.into_iter().enumerate() {
            // Each Unicode scalar retains its own CID and ToUnicode entry even
            // when multiple unsupported scalars share the fallback GID.
            let gid = self.glyph_id(character);
            let cid = u16::try_from(index + 1).map_err(|_| {
                Error::PdfOverlay("Too many distinct characters for CID encoding".to_string())
            })?;
            by_character.insert(character, cid);
            glyphs.push(EncodedGlyph {
                cid,
                gid,
                character,
            });
        }

        Ok(FontEncoding {
            by_character,
            glyphs,
        })
    }

    /// Convert text to a hex string of assigned CIDs for a PDF content stream.
    pub(super) fn text_to_hex_cids(text: &str, encoding: &FontEncoding) -> Result<String> {
        text.chars()
            .try_fold(String::new(), |mut output, character| {
                let cid = encoding.by_character.get(&character).ok_or_else(|| {
                    Error::PdfOverlay(format!(
                        "No CID assigned for emitted character U+{:04X}",
                        u32::from(character)
                    ))
                })?;
                write!(output, "{cid:04X}")
                    .map_err(|error| Error::PdfOverlay(format!("Failed to encode CID: {error}")))?;
                Ok(output)
            })
    }

    /// Embed this font into a PDF document and add it to a page's resources.
    pub(super) fn embed_in_document(
        &self,
        doc: &mut Document,
        page_id: ObjectId,
        encoding: &FontEncoding,
    ) -> Result<&'static str> {
        let font_file_id = self.create_font_file(doc);
        let font_descriptor_id = self.create_font_descriptor(doc, font_file_id);
        let cid_to_gid_id = Self::create_cid_to_gid_map(doc, encoding);
        let cid_font_id = self.create_cid_font(doc, font_descriptor_id, cid_to_gid_id, encoding);
        let to_unicode_id = Self::create_to_unicode_cmap(doc, encoding);
        let type0_font_id = self.create_type0_font(doc, cid_font_id, to_unicode_id);

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
        let [x_min, y_min, x_max, y_max] = self.bounding_box;
        let dict = lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"FontDescriptor".to_vec())),
            ("FontName", Object::Name(b"NotoSerif".to_vec())),
            (
                "FontFamily",
                Object::String(b"Noto Serif".to_vec(), lopdf::StringFormat::Literal),
            ),
            ("Flags", Object::Integer(32)), // Nonsymbolic
            (
                "FontBBox",
                Object::Array(vec![
                    Object::Integer(i64::from(x_min)),
                    Object::Integer(i64::from(y_min)),
                    Object::Integer(i64::from(x_max)),
                    Object::Integer(i64::from(y_max)),
                ]),
            ),
            ("ItalicAngle", Object::Integer(0)),
            ("Ascent", Object::Integer(i64::from(self.ascender))),
            ("Descent", Object::Integer(i64::from(self.descender))),
            ("CapHeight", Object::Integer(i64::from(self.capital_height))),
            ("StemV", Object::Integer(90)), // Approximate value for serif
            ("FontFile2", Object::Reference(font_file_id)),
        ]);

        doc.add_object(Object::Dictionary(dict))
    }

    /// Create the CIDFont dictionary with per-CID width information.
    fn create_cid_font(
        &self,
        doc: &mut Document,
        font_descriptor_id: ObjectId,
        cid_to_gid_id: ObjectId,
        encoding: &FontEncoding,
    ) -> ObjectId {
        let widths_array = self.build_widths_array(encoding);
        let default_width = self.scale_width(self.glyph_width(self.glyph_id(' ')));

        let dict = lopdf::Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"CIDFontType2".to_vec())),
            ("BaseFont", Object::Name(b"NotoSerif".to_vec())),
            (
                "CIDSystemInfo",
                Object::Dictionary(lopdf::Dictionary::from_iter([
                    (
                        "Registry",
                        Object::String(b"Adobe".to_vec(), lopdf::StringFormat::Literal),
                    ),
                    (
                        "Ordering",
                        Object::String(b"Identity".to_vec(), lopdf::StringFormat::Literal),
                    ),
                    ("Supplement", Object::Integer(0)),
                ])),
            ),
            ("FontDescriptor", Object::Reference(font_descriptor_id)),
            ("DW", Object::Integer(default_width)),
            ("W", Object::Array(widths_array)),
            ("CIDToGIDMap", Object::Reference(cid_to_gid_id)),
        ]);

        doc.add_object(Object::Dictionary(dict))
    }

    /// Scale a font-unit width to PDF's 1000-unit system.
    fn scale_width(&self, width: u16) -> i64 {
        // PDF expects widths in 1/1000ths of text space
        // TrueType widths are in font design units (e.g., 2048 per em)
        (i64::from(width) * 1000) / i64::from(self.units_per_em)
    }

    /// Build the W array for every emitted CID using its TrueType glyph width.
    fn build_widths_array(&self, encoding: &FontEncoding) -> Vec<Object> {
        if encoding.glyphs.is_empty() {
            return Vec::new();
        }

        let widths = encoding
            .glyphs
            .iter()
            .map(|glyph| Object::Integer(self.scale_width(self.glyph_width(glyph.gid))))
            .collect();
        vec![Object::Integer(1), Object::Array(widths)]
    }

    /// Build the binary CIDToGIDMap stream. Entry N is the big-endian GID for CID N.
    fn create_cid_to_gid_map(doc: &mut Document, encoding: &FontEncoding) -> ObjectId {
        let mut map = Vec::with_capacity((encoding.glyphs.len() + 1) * 2);
        map.extend_from_slice(&0_u16.to_be_bytes());
        for glyph in &encoding.glyphs {
            map.extend_from_slice(&glyph.gid.to_be_bytes());
        }
        let stream = Stream::new(lopdf::Dictionary::new(), map).with_compression(true);
        doc.add_object(Object::Stream(stream))
    }

    /// Create a ToUnicode CMap for text extraction/copy-paste support.
    fn create_to_unicode_cmap(doc: &mut Document, encoding: &FontEncoding) -> ObjectId {
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin\n\
             12 dict begin\n\
             begincmap\n\
             /CIDSystemInfo <<\n\
             /Registry (Adobe)\n\
             /Ordering (UCS)\n\
             /Supplement 0\n\
             >> def\n\
             /CMapName /Adobe-Identity-UCS def\n\
             /CMapType 2 def\n\
             1 begincodespacerange\n\
             <0000> <FFFF>\n\
             endcodespacerange\n",
        );

        let mut mappings = encoding.glyphs.iter();
        while mappings.len() != 0 {
            let count = mappings.len().min(100);
            let _ = writeln!(cmap, "{count} beginbfchar");
            for glyph in mappings.by_ref().take(count) {
                let mut utf16 = [0_u16; 2];
                let encoded = glyph.character.encode_utf16(&mut utf16);
                let cid = glyph.cid;
                let _ = write!(cmap, "<{cid:04X}> <");
                for unit in encoded {
                    let _ = write!(cmap, "{unit:04X}");
                }
                cmap.push_str(">\n");
            }
            cmap.push_str("endbfchar\n");
        }

        cmap.push_str(
            "endcmap\n\
             CMapName currentdict /CMap defineresource pop\n\
             end\n\
             end",
        );

        let stream = Stream::new(lopdf::Dictionary::new(), cmap.into_bytes());
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
            (
                "DescendantFonts",
                Object::Array(vec![Object::Reference(cid_font_id)]),
            ),
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
        let mut resources = Self::resolve_resources(doc, page_id)?;

        // Resolve the Font dictionary within Resources (also may be a reference)
        let mut fonts = match resources.get(b"Font").ok() {
            Some(Object::Dictionary(dictionary)) => dictionary.clone(),
            Some(Object::Reference(object_id)) => match doc.get_object(*object_id) {
                Ok(Object::Dictionary(dictionary)) => dictionary.clone(),
                Ok(_) => {
                    return Err(Error::Lopdf(
                        "Resources Font object is not a dictionary".to_string(),
                    ));
                }
                Err(error) => {
                    return Err(Error::Lopdf(format!(
                        "Failed to resolve Font dictionary {object_id:?}: {error}"
                    )));
                }
            },
            Some(_) => {
                return Err(Error::Lopdf(
                    "Resources Font entry is not a dictionary".to_string(),
                ));
            }
            None => lopdf::Dictionary::new(),
        };

        // Add our font as FTrans
        fonts.set("FTrans", Object::Reference(font_id));
        resources.set("Font", Object::Dictionary(fonts));

        // Set the updated Resources back on the page (as inline dict to capture our changes)
        let page = doc
            .get_object_mut(page_id)
            .map_err(|e| Error::Lopdf(format!("Failed to get page: {e}")))?;

        let Object::Dictionary(page_dict) = page else {
            return Err(Error::Lopdf("Page object is not a dictionary".to_string()));
        };
        page_dict.set("Resources", Object::Dictionary(resources));

        Ok(())
    }

    /// Resolve the Resources dictionary for a page, handling indirect references
    /// and inheritance from parent Pages nodes.
    ///
    /// PDF pages can have Resources as:
    /// - An inline dictionary: `/Resources << /Font << ... >> >>`
    /// - An indirect reference: `/Resources 5 0 R`
    /// - Inherited from parent Pages node (common in complex PDFs like archive.org)
    fn resolve_resources(doc: &Document, page_id: ObjectId) -> Result<lopdf::Dictionary> {
        let page = doc
            .get_object(page_id)
            .map_err(|e| Error::Lopdf(format!("Failed to get page: {e}")))?;
        let Object::Dictionary(page_dict) = page else {
            return Err(Error::Lopdf("Page object is not a dictionary".to_string()));
        };

        if let Ok(resources) = page_dict.get(b"Resources") {
            return Self::resolve_dict_object(doc, resources)?
                .ok_or_else(|| Error::Lopdf("Page Resources is not a dictionary".to_string()));
        }

        let Some(Object::Reference(parent_id)) = page_dict.get(b"Parent").ok() else {
            return Ok(lopdf::Dictionary::new());
        };

        Self::resolve_inherited_resources(doc, *parent_id)?
            .map_or_else(|| Ok(lopdf::Dictionary::new()), Ok)
    }

    /// Resolve an object that should be a Dictionary (handles References).
    fn resolve_dict_object(doc: &Document, object: &Object) -> Result<Option<lopdf::Dictionary>> {
        match object {
            Object::Dictionary(dictionary) => Ok(Some(dictionary.clone())),
            Object::Reference(object_id) => match doc.get_object(*object_id) {
                Ok(Object::Dictionary(dictionary)) => Ok(Some(dictionary.clone())),
                Ok(_) => Ok(None),
                Err(error) => Err(Error::Lopdf(format!(
                    "Failed to resolve dictionary {object_id:?}: {error}"
                ))),
            },
            _ => Ok(None),
        }
    }

    /// Walk up the Pages tree to find inherited Resources.
    fn resolve_inherited_resources(
        doc: &Document,
        mut parent_id: ObjectId,
    ) -> Result<Option<lopdf::Dictionary>> {
        let mut visited = HashSet::new();

        loop {
            if !visited.insert(parent_id) {
                return Err(Error::Lopdf(format!(
                    "Cycle in page Parent chain at {parent_id:?}"
                )));
            }

            let parent_object = doc.get_object(parent_id).map_err(|error| {
                Error::Lopdf(format!("Failed to get Pages node {parent_id:?}: {error}"))
            })?;
            let Object::Dictionary(parent) = parent_object else {
                return Err(Error::Lopdf("Pages node is not a dictionary".to_string()));
            };

            if let Ok(resources) = parent.get(b"Resources") {
                return Self::resolve_dict_object(doc, resources)?.map_or_else(
                    || {
                        Err(Error::Lopdf(
                            "Inherited Resources is not a dictionary".to_string(),
                        ))
                    },
                    |dictionary| Ok(Some(dictionary)),
                );
            }

            match parent.get(b"Parent").ok() {
                Some(Object::Reference(grandparent_id)) => parent_id = *grandparent_id,
                Some(_) => {
                    return Err(Error::Lopdf(
                        "Pages Parent is not an indirect reference".to_string(),
                    ));
                }
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        let encoding = font.encoding_for_characters("A".chars()).unwrap();
        let hex = EmbeddedFont::text_to_hex_cids("A", &encoding).unwrap();
        // Should be 4 hex digits for one character
        assert_eq!(hex.len(), 4);
    }
}
