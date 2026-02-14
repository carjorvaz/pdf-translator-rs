use std::path::Path;
use std::sync::Arc;

use mupdf::{Document as MuDocument, MetadataName};

use crate::error::{Error, Result};

/// Thread-safe wrapper around a PDF document
pub struct PdfDocument {
    /// The raw PDF bytes (kept for potential re-processing)
    bytes: Arc<Vec<u8>>,
    /// Cached metadata
    metadata: DocumentMetadata,
    /// Number of pages
    page_count: usize,
    /// Content-based cache ID (MD5 hex), computed once on load
    cache_id: String,
}

/// Document metadata
#[derive(Debug, Clone, Default)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
}

impl PdfDocument {
    /// Open a PDF from bytes
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();

        // Open document to extract metadata and page count
        let doc = MuDocument::from_bytes(&bytes, "")
            .map_err(|e| Error::PdfOpen(format!("Failed to parse PDF: {e}")))?;

        let page_count = doc.page_count()
            .map_err(|e| Error::PdfOpen(format!("Failed to get page count: {e}")))?;

        // Extract metadata - mupdf returns empty string if not present
        let get_meta = |name| -> Option<String> {
            doc.metadata(name).ok().filter(|s| !s.is_empty())
        };

        let metadata = DocumentMetadata {
            title: get_meta(MetadataName::Title),
            author: get_meta(MetadataName::Author),
            subject: get_meta(MetadataName::Subject),
            keywords: get_meta(MetadataName::Keywords),
            creator: get_meta(MetadataName::Creator),
            producer: get_meta(MetadataName::Producer),
            creation_date: get_meta(MetadataName::CreationDate),
            modification_date: get_meta(MetadataName::ModDate),
        };

        let cache_id = format!("{:x}", md5::compute(&bytes));

        Ok(Self {
            bytes: Arc::new(bytes),
            metadata,
            page_count: usize::try_from(page_count).unwrap_or(0),
            cache_id,
        })
    }

    /// Open a PDF from a file path
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref()).map_err(|e| {
            Error::PdfOpen(format!("Failed to read file {}: {}", path.as_ref().display(), e))
        })?;
        Self::from_bytes(bytes)
    }

    /// Get document metadata
    pub const fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
    }

    /// Get number of pages
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Get raw PDF bytes as a slice.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Get raw PDF bytes as a reference-counted pointer.
    ///
    /// Use this when you need to share the bytes across threads or store them
    /// without copying. This is an O(1) operation that only increments the
    /// reference count.
    pub fn bytes_arc(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.bytes)
    }

    /// Open the document for operations (creates a temporary handle)
    pub(crate) fn open_document(&self) -> Result<MuDocument> {
        MuDocument::from_bytes(&self.bytes, "")
            .map_err(|e| Error::PdfOpen(format!("Failed to open document: {e}")))
    }

    /// Cache key component derived from document content.
    ///
    /// MD5 hash of the PDF bytes, computed once on load.
    pub fn cache_id(&self) -> &str {
        &self.cache_id
    }
}

impl Clone for PdfDocument {
    /// Clone the document efficiently.
    ///
    /// This is O(1) - it only clones the `Arc` pointer to the underlying bytes,
    /// not the bytes themselves. The metadata is also cloned (small struct).
    fn clone(&self) -> Self {
        Self {
            bytes: Arc::clone(&self.bytes),
            metadata: self.metadata.clone(),
            page_count: self.page_count,
            cache_id: self.cache_id.clone(),
        }
    }
}

impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("page_count", &self.page_count)
            .field("metadata", &self.metadata)
            .field("bytes_len", &self.bytes.len())
            .finish()
    }
}
