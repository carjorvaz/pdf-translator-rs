mod traits;
mod openai;

pub use traits::{Translator, TranslatorInfo};
pub use openai::OpenAiTranslator;

use crate::config::TranslatorConfig;
use crate::error::Result;
use std::sync::Arc;

/// Create a translator from configuration
pub fn create_translator(config: &TranslatorConfig) -> Result<Arc<dyn Translator>> {
    let translator = OpenAiTranslator::new(
        config.api_base.clone(),
        config.api_key.clone(),
        config.model.clone(),
        config.retry_count,
        config.retry_delay_ms,
    );

    Ok(Arc::new(translator))
}
