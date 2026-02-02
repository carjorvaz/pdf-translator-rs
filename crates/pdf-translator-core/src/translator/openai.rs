use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn, error};

use crate::config::Lang;
use crate::error::{Error, Result};
use super::traits::{Translator, TranslatorInfo};

/// Default number of retry attempts
pub const DEFAULT_RETRY_COUNT: u32 = 3;
/// Default delay between retries in milliseconds
pub const DEFAULT_RETRY_DELAY_MS: u64 = 1000;

/// OpenAI-compatible API translator
/// Works with: llama.cpp server, Ollama, DeepSeek, OpenAI, etc.
pub struct OpenAiTranslator {
    client: Client,
    /// Base URL for the API (e.g., "http://localhost:8080/v1")
    pub api_base: String,
    /// Optional API key for authentication
    pub api_key: Option<String>,
    /// Model identifier
    pub model: String,
    /// Number of retry attempts
    pub retry_count: u32,
    /// Delay between retries in milliseconds
    pub retry_delay_ms: u64,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

impl OpenAiTranslator {
    /// Create a new OpenAI translator with all options.
    ///
    /// # Panics
    /// Panics if the HTTP client cannot be created, which should only happen
    /// in extreme circumstances (e.g., TLS backend unavailable on the system).
    #[allow(clippy::expect_used)]
    pub fn new(
        api_base: String,
        api_key: Option<String>,
        model: String,
        retry_count: u32,
        retry_delay_ms: u64,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            api_base,
            api_key,
            model,
            retry_count,
            retry_delay_ms,
        }
    }

    /// Create a new OpenAI translator with default retry settings.
    ///
    /// # Panics
    /// Panics if the HTTP client cannot be created.
    pub fn with_defaults(api_base: String, api_key: Option<String>, model: String) -> Self {
        Self::new(api_base, api_key, model, DEFAULT_RETRY_COUNT, DEFAULT_RETRY_DELAY_MS)
    }

    /// Create translation prompt
    fn create_prompt(text: &str, source: &Lang, target: &Lang) -> String {
        let source_hint = if source.as_str() == "auto" {
            String::new()
        } else {
            format!(" from {}", source_language_name(source))
        };
        format!(
            "Translate the following text{} into {}. Output only the translation, no explanations.\n\nText: \"{}\"",
            source_hint,
            target_language_name(target),
            text
        )
    }

    /// Make API request with retry logic
    async fn request_with_retry(&self, text: &str, source: &Lang, target: &Lang) -> Result<String> {
        let url = format!("{}/chat/completions", self.api_base.trim_end_matches('/'));
        let prompt = Self::create_prompt(text, source, target);

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt,
            }],
            temperature: Some(0.3), // Lower temperature for more consistent translations
            max_tokens: None,
        };

        let mut last_error = None;

        for attempt in 0..self.retry_count {
            debug!(
                "Translation request attempt {}/{} to {}",
                attempt + 1,
                self.retry_count,
                url
            );

            let mut req = self.client.post(&url).json(&request);

            // Add API key if configured
            if let Some(ref key) = self.api_key {
                req = req.header("Authorization", format!("Bearer {key}"));
            }

            match req.send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        match response.json::<ChatResponse>().await {
                            Ok(chat_response) => {
                                if let Some(choice) = chat_response.choices.first() {
                                    let translated = choice.message.content.trim();
                                    // Remove quotes if the model wrapped the response
                                    let translated = translated
                                        .trim_start_matches('"')
                                        .trim_end_matches('"')
                                        .to_string();
                                    return Ok(translated);
                                }
                                last_error = Some(Error::TranslationInvalidResponse(
                                    "No choices in response".to_string(),
                                ));
                            }
                            Err(e) => {
                                warn!("Failed to parse response: {}", e);
                                last_error = Some(Error::TranslationInvalidResponse(e.to_string()));
                            }
                        }
                    } else if response.status().as_u16() == 429 {
                        // Rate limited
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse().ok());

                        warn!("Rate limited, retry after {:?}s", retry_after);
                        last_error = Some(Error::TranslationRateLimited { retry_after });

                        // Wait longer on rate limit
                        let wait_time = retry_after.unwrap_or(5) * 1000;
                        tokio::time::sleep(Duration::from_millis(wait_time)).await;
                        continue;
                    } else {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        warn!("API error: {} - {}", status, body);
                        last_error = Some(Error::TranslationRequest(format!(
                            "HTTP {status}: {body}"
                        )));
                    }
                }
                Err(e) => {
                    warn!("Request failed: {}", e);
                    if e.is_timeout() {
                        last_error = Some(Error::TranslationTimeout);
                    } else {
                        last_error = Some(Error::TranslationRequest(e.to_string()));
                    }
                }
            }

            // Wait before retry
            if attempt < self.retry_count - 1 {
                tokio::time::sleep(Duration::from_millis(self.retry_delay_ms)).await;
            }
        }

        error!("Translation failed after {} attempts", self.retry_count);
        Err(last_error.unwrap_or(Error::TranslationMaxRetriesExceeded))
    }
}

#[async_trait]
impl Translator for OpenAiTranslator {
    fn info(&self) -> TranslatorInfo {
        TranslatorInfo {
            name: "OpenAI Compatible",
            requires_api_key: false, // Optional for local servers
            supports_auto_detect: true,
        }
    }

    async fn translate(&self, text: &str, source: &Lang, target: &Lang) -> Result<String> {
        // Skip empty text
        if text.trim().is_empty() {
            return Ok(text.to_string());
        }

        // Skip if source and target are the same
        if source.as_str() == target.as_str() && source.as_str() != "auto" {
            return Ok(text.to_string());
        }

        self.request_with_retry(text, source, target).await
    }

    fn is_available(&self) -> bool {
        // For local servers, we don't require an API key
        true
    }
}

/// Convert language code to human-readable name for prompts
fn language_name(lang: &Lang) -> &'static str {
    match lang.as_str() {
        "en" => "English",
        "zh-CN" => "Simplified Chinese",
        "zh-TW" => "Traditional Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        "es" => "Spanish",
        "fr" => "French",
        "de" => "German",
        "it" => "Italian",
        "pt" => "Portuguese",
        "ru" => "Russian",
        "ar" => "Arabic",
        "hi" => "Hindi",
        "th" => "Thai",
        "vi" => "Vietnamese",
        // For unknown languages, the LLM should still understand most ISO codes
        _ => "the specified language",
    }
}

/// Alias for backwards compatibility and clarity in prompts
fn target_language_name(lang: &Lang) -> &'static str {
    language_name(lang)
}

fn source_language_name(lang: &Lang) -> &'static str {
    language_name(lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_name() {
        assert_eq!(language_name(&Lang::new("en")), "English");
        assert_eq!(language_name(&Lang::new("zh-CN")), "Simplified Chinese");
        assert_eq!(language_name(&Lang::new("unknown")), "the specified language");
    }
}
