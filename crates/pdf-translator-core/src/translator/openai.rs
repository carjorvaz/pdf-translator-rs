use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, warn};

use super::traits::{Translator, TranslatorInfo};
use crate::config::{Lang, TranslatorCacheIdentity};
use crate::error::{Error, Result};

/// Default number of retry attempts
pub const DEFAULT_RETRY_COUNT: u32 = 3;
/// Default delay between retries in milliseconds
pub const DEFAULT_RETRY_DELAY_MS: u64 = 1000;

const MAX_SUCCESS_BODY_BYTES: usize = 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_RETRY_AFTER_SECONDS: u64 = 300;
const DEFAULT_RATE_LIMIT_DELAY_SECONDS: u64 = 5;

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
    finish_reason: Option<String>,
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
        Self::new(
            api_base,
            api_key,
            model,
            DEFAULT_RETRY_COUNT,
            DEFAULT_RETRY_DELAY_MS,
        )
    }

    fn normalized_endpoint(&self) -> String {
        let trimmed = self.api_base.trim();
        if let Ok(mut url) = reqwest::Url::parse(trimmed) {
            let path = url.path().trim_end_matches('/').to_string();
            url.set_path(if path.is_empty() { "/" } else { &path });
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            return url.as_str().trim_end_matches('/').to_string();
        }

        trimmed.trim_end_matches('/').to_string()
    }

    async fn read_body_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
        let limit_u64 = u64::try_from(limit).map_err(|_| {
            Error::TranslationInvalidResponse("response size limit is invalid".to_string())
        })?;
        if response
            .content_length()
            .is_some_and(|length| length > limit_u64)
        {
            return Err(Error::TranslationInvalidResponse(
                "translation API response exceeded the size limit".to_string(),
            ));
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            Error::TranslationRequest("failed to read translation API response".to_string())
        })? {
            let new_len = body.len().checked_add(chunk.len()).ok_or_else(|| {
                Error::TranslationInvalidResponse(
                    "translation API response exceeded the size limit".to_string(),
                )
            })?;
            if new_len > limit {
                return Err(Error::TranslationInvalidResponse(
                    "translation API response exceeded the size limit".to_string(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn parse_completion(body: &[u8]) -> Result<String> {
        let response: ChatResponse = serde_json::from_slice(body).map_err(|_| {
            Error::TranslationInvalidResponse("translation API returned malformed JSON".to_string())
        })?;
        let choice = response.choices.first().ok_or_else(|| {
            Error::TranslationInvalidResponse(
                "translation API response contained no choices".to_string(),
            )
        })?;
        match choice.finish_reason.as_deref() {
            Some("stop") => {}
            Some("length") => {
                return Err(Error::TranslationInvalidResponse(
                    "translation API response was truncated".to_string(),
                ));
            }
            Some("content_filter") => {
                return Err(Error::TranslationInvalidResponse(
                    "translation API response was content-filtered".to_string(),
                ));
            }
            _ => {
                return Err(Error::TranslationInvalidResponse(
                    "translation API response was incomplete".to_string(),
                ));
            }
        }

        let translated = choice
            .message
            .content
            .trim()
            .trim_start_matches('"')
            .trim_end_matches('"')
            .trim();
        if translated.is_empty() {
            return Err(Error::TranslationInvalidResponse(
                "translation API response was blank".to_string(),
            ));
        }
        Ok(translated.to_string())
    }

    /// Create translation prompt
    fn create_prompt(text: &str, source: &Lang, target: &Lang) -> String {
        let source_hint = if source.as_str() == "auto" {
            String::new()
        } else {
            format!(" from {}", language_name(source))
        };
        format!(
            "Translate the following text{} into {}. Output only the translation, no explanations.\n\nText: \"{}\"",
            source_hint,
            language_name(target),
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
            temperature: Some(0.3),
            max_tokens: None,
        };
        let attempts = self.retry_count.max(1);
        let mut last_error = None;

        for attempt in 0..attempts {
            debug!(
                "Translation request attempt {}/{} to {}",
                attempt + 1,
                attempts,
                url
            );

            let mut req = self.client.post(&url).json(&request);
            if let Some(key) = &self.api_key {
                req = req.header("Authorization", format!("Bearer {key}"));
            }

            match req.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        match Self::read_body_limited(response, MAX_SUCCESS_BODY_BYTES).await {
                            Ok(body) => match Self::parse_completion(&body) {
                                Ok(translated) => return Ok(translated),
                                Err(error) => last_error = Some(error),
                            },
                            Err(error) => last_error = Some(error),
                        }
                    } else if status.as_u16() == 429 {
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<u64>().ok())
                            .map(|seconds| seconds.min(MAX_RETRY_AFTER_SECONDS));
                        let _ = Self::read_body_limited(response, MAX_ERROR_BODY_BYTES).await;
                        warn!("Translation API rate limited the request");
                        last_error = Some(Error::TranslationRateLimited { retry_after });

                        if attempt + 1 < attempts {
                            let delay = retry_after
                                .unwrap_or(DEFAULT_RATE_LIMIT_DELAY_SECONDS)
                                .min(MAX_RETRY_AFTER_SECONDS);
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                        }
                        continue;
                    } else {
                        let _ = Self::read_body_limited(response, MAX_ERROR_BODY_BYTES).await;
                        warn!("Translation API returned HTTP {}", status);
                        last_error = Some(Error::TranslationRequest(format!(
                            "upstream returned HTTP {status}"
                        )));
                    }
                }
                Err(error) => {
                    warn!("Translation request failed: {}", error);
                    last_error = Some(if error.is_timeout() {
                        Error::TranslationTimeout
                    } else {
                        Error::TranslationRequest(
                            "translation API request could not be completed".to_string(),
                        )
                    });
                }
            }

            if attempt + 1 < attempts {
                tokio::time::sleep(Duration::from_millis(self.retry_delay_ms)).await;
            }
        }

        error!("Translation failed after {} attempts", attempts);
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

    fn cache_identity(&self) -> TranslatorCacheIdentity {
        TranslatorCacheIdentity::new(
            "openai-compatible",
            self.normalized_endpoint(),
            self.model.clone(),
        )
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

/// Convert a language code to a human-readable prompt value when known.
fn language_name(lang: &Lang) -> &str {
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
        _ => lang.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_name() {
        assert_eq!(language_name(&Lang::new("en")), "English");
        assert_eq!(language_name(&Lang::new("zh-CN")), "Simplified Chinese");
        assert_eq!(language_name(&Lang::new("unknown")), "unknown");
    }
}
