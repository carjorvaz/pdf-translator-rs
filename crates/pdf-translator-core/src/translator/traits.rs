use async_trait::async_trait;
use crate::config::Lang;
use crate::error::Result;

/// Information about a translator backend
#[derive(Debug, Clone)]
pub struct TranslatorInfo {
    /// Human-readable name
    pub name: &'static str,
    /// Whether this translator requires an API key
    pub requires_api_key: bool,
    /// Whether this translator supports auto-detection of source language
    pub supports_auto_detect: bool,
}

/// Trait for translation backends
#[async_trait]
pub trait Translator: Send + Sync {
    /// Get information about this translator
    fn info(&self) -> TranslatorInfo;

    /// Get the translator name (convenience method)
    fn name(&self) -> &'static str {
        self.info().name
    }

    /// Translate text from source language to target language
    async fn translate(
        &self,
        text: &str,
        source: &Lang,
        target: &Lang,
    ) -> Result<String>;

    /// Check if the translator is available (e.g., API key configured)
    fn is_available(&self) -> bool {
        true
    }
}
