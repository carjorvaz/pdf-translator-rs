use thiserror::Error;

/// Unified error type for pdf-translator-core
///
/// This enum encompasses all error cases that can occur in the library:
/// - PDF operations (opening, reading, rendering, saving)
/// - Translation operations (API requests, responses, rate limiting)
/// - Cache operations (initialization, reading, writing)
/// - Configuration operations (loading, validation)
/// - General I/O operations
#[derive(Error, Debug)]
pub enum Error {
    // ==========================================================================
    // PDF Errors
    // ==========================================================================
    /// Failed to open or parse a PDF file
    #[error("failed to open PDF: {0}")]
    PdfOpen(String),

    /// Failed to load the PDFium library
    #[error("failed to load PDFium library: {0}")]
    PdfiumLoad(String),

    /// Invalid page number requested
    #[error("invalid page number {page} (document has {total} pages)")]
    PdfInvalidPage { page: usize, total: usize },

    /// Failed to extract text from a PDF page
    #[error("failed to extract text from page {page}: {reason}")]
    PdfTextExtraction { page: usize, reason: String },

    /// Failed to render a PDF page
    #[error("failed to render page {page}: {reason}")]
    PdfRender { page: usize, reason: String },

    /// Failed to create a PDF overlay
    #[error("failed to create PDF overlay: {0}")]
    PdfOverlay(String),

    /// Failed to save a PDF
    #[error("failed to save PDF: {0}")]
    PdfSave(String),

    /// Error from the lopdf library
    #[error("lopdf error: {0}")]
    Lopdf(String),

    // ==========================================================================
    // Translation Errors
    // ==========================================================================
    /// Translation API request failed
    #[error("translation API request failed: {0}")]
    TranslationRequest(String),

    /// Invalid response from translation API
    #[error("invalid translation API response: {0}")]
    TranslationInvalidResponse(String),

    /// Rate limited by translation API
    #[error("translation rate limited{}", retry_after.map(|s| format!(", retry after {s} seconds")).unwrap_or_default())]
    TranslationRateLimited { retry_after: Option<u64> },

    /// API key not configured for translation service
    #[error("translation API key not configured")]
    TranslationMissingApiKey,

    /// Unsupported language for translation
    #[error("unsupported language for translation: {0}")]
    TranslationUnsupportedLanguage(String),

    /// Translation request timed out
    #[error("translation request timed out")]
    TranslationTimeout,

    /// Maximum retry attempts exceeded for translation
    #[error("translation failed after maximum retries")]
    TranslationMaxRetriesExceeded,

    // ==========================================================================
    // Cache Errors
    // ==========================================================================
    /// Failed to initialize the cache
    #[error("failed to initialize cache: {0}")]
    CacheInit(String),

    /// Failed to read from cache
    #[error("failed to read from cache: {0}")]
    CacheRead(String),

    /// Failed to write to cache
    #[error("failed to write to cache: {0}")]
    CacheWrite(String),

    /// Failed to generate cache key
    #[error("cache key generation failed: {0}")]
    CacheKeyGeneration(String),

    // ==========================================================================
    // Configuration Errors
    // ==========================================================================
    /// Failed to load configuration file
    #[error("failed to load config: {0}")]
    ConfigLoad(String),

    /// Invalid configuration value
    #[error("invalid config value for '{field}': {reason}")]
    ConfigInvalid { field: String, reason: String },

    /// Missing required configuration field
    #[error("missing required config field: {0}")]
    ConfigMissing(String),

    // ==========================================================================
    // I/O Errors
    // ==========================================================================
    /// General I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
