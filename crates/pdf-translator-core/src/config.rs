use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Language codes following ISO 639-1 with regional variants
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lang(pub String);

impl Lang {
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// Serde default functions for common languages
fn default_source_lang() -> Lang {
    Lang::new("fr")
}

fn default_target_lang() -> Lang {
    Lang::new("en")
}

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for Lang {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for Lang {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Text color for translation overlay
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl TextColor {
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    pub const fn dark_red() -> Self {
        Self::new(0.8, 0.0, 0.0)
    }

    pub const fn black() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }

    pub const fn blue() -> Self {
        Self::new(0.0, 0.0, 0.8)
    }

    pub const fn dark_green() -> Self {
        Self::new(0.0, 0.5, 0.0)
    }

    pub const fn purple() -> Self {
        Self::new(0.5, 0.0, 0.5)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "darkred" | "dark_red" | "dark-red" => Some(Self::dark_red()),
            "black" => Some(Self::black()),
            "blue" => Some(Self::blue()),
            "darkgreen" | "dark_green" | "dark-green" => Some(Self::dark_green()),
            "purple" => Some(Self::purple()),
            _ => None,
        }
    }

    /// Convert to RGB bytes (0-255)
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn to_rgb_bytes(&self) -> (u8, u8, u8) {
        // Values are clamped to 0.0-1.0 range, so conversion is safe
        (
            (self.r.clamp(0.0, 1.0) * 255.0) as u8,
            (self.g.clamp(0.0, 1.0) * 255.0) as u8,
            (self.b.clamp(0.0, 1.0) * 255.0) as u8,
        )
    }

    /// Convert to CSS rgb() string
    pub fn to_css(&self) -> String {
        let (r, g, b) = self.to_rgb_bytes();
        format!("rgb({r}, {g}, {b})")
    }
}

impl Default for TextColor {
    fn default() -> Self {
        Self::dark_red()
    }
}

/// Translator backend configuration for OpenAI-compatible APIs.
///
/// Supports llama.cpp, Ollama, DeepSeek, OpenAI, and any other OpenAI-compatible API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatorConfig {
    pub api_base: String,
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default = "default_retry_count")]
    pub retry_count: u32,
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,
}

impl TranslatorConfig {
    /// Create a new translator config
    pub fn new(
        api_base: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            api_key,
            model: model.into(),
            retry_count: default_retry_count(),
            retry_delay_ms: default_retry_delay_ms(),
        }
    }
}

const fn default_retry_count() -> u32 {
    3
}

const fn default_retry_delay_ms() -> u64 {
    1000
}

impl Default for TranslatorConfig {
    fn default() -> Self {
        Self {
            api_base: "http://localhost:8080/v1".to_string(),
            api_key: None,
            model: "default_model".to_string(),
            retry_count: default_retry_count(),
            retry_delay_ms: default_retry_delay_ms(),
        }
    }
}

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Enable memory cache
    #[serde(default = "default_true")]
    pub memory_enabled: bool,

    /// Maximum memory cache entries
    #[serde(default = "default_memory_max_entries")]
    pub memory_max_entries: u64,

    /// Memory cache TTL in seconds (0 = no expiry)
    #[serde(default)]
    pub memory_ttl_seconds: u64,

    /// Enable disk cache
    #[serde(default = "default_true")]
    pub disk_enabled: bool,

    /// Disk cache directory (defaults to .cache/pdf-translator)
    pub disk_path: Option<PathBuf>,
}

const fn default_true() -> bool {
    true
}

const fn default_memory_max_entries() -> u64 {
    1000
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            memory_enabled: true,
            memory_max_entries: 1000,
            memory_ttl_seconds: 0,
            disk_enabled: true,
            disk_path: None,
        }
    }
}

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Source language
    #[serde(default = "default_source_lang")]
    pub source_lang: Lang,

    /// Target language
    #[serde(default = "default_target_lang")]
    pub target_lang: Lang,

    /// Translation text color
    #[serde(default)]
    pub text_color: TextColor,

    /// Translator backend configuration
    #[serde(default)]
    pub translator: TranslatorConfig,

    /// Cache configuration
    #[serde(default)]
    pub cache: CacheConfig,

    /// PDF rendering scale factor (default: 2.0 for high DPI)
    #[serde(default = "default_render_scale")]
    pub render_scale: f32,

    /// Pages to load per batch in web UI
    #[serde(default = "default_pages_per_load")]
    pub pages_per_load: usize,
}

const fn default_render_scale() -> f32 {
    2.0
}

const fn default_pages_per_load() -> usize {
    2
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            source_lang: default_source_lang(),
            target_lang: default_target_lang(),
            text_color: TextColor::default(),
            translator: TranslatorConfig::default(),
            cache: CacheConfig::default(),
            render_scale: default_render_scale(),
            pages_per_load: default_pages_per_load(),
        }
    }
}

impl AppConfig {
    /// Load configuration from file
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, crate::error::Error> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(|e| {
            crate::error::Error::ConfigLoad(format!(
                "Failed to read config file {}: {}",
                path.as_ref().display(),
                e
            ))
        })?;

        toml::from_str(&content).map_err(|e| {
            crate::error::Error::ConfigLoad(format!("Failed to parse config: {e}"))
        })
    }

    /// Load from default locations (~/.config/pdf-translator/config.toml, ./config.toml)
    pub fn load() -> Self {
        // Try user config
        if let Some(config_dir) = crate::util::config_dir() {
            let user_config = config_dir.join("pdf-translator").join("config.toml");
            if user_config.exists() {
                match Self::from_file(&user_config) {
                    Ok(config) => {
                        tracing::debug!("Loaded config from {}", user_config.display());
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load {}: {}", user_config.display(), e);
                    }
                }
            }
        }

        // Try local config
        let local_config = std::path::PathBuf::from("config.toml");
        if local_config.exists() {
            match Self::from_file(&local_config) {
                Ok(config) => {
                    tracing::debug!("Loaded config from ./config.toml");
                    return config;
                }
                Err(e) => {
                    tracing::warn!("Failed to load ./config.toml: {}", e);
                }
            }
        }

        // Return defaults
        tracing::debug!("No config file found, using defaults");
        Self::default()
    }
}

/// A language option for UI dropdowns
#[derive(Debug, Clone)]
pub struct LanguageOption {
    /// ISO language code (e.g., "en", "fr", "zh-CN")
    pub code: &'static str,
    /// Display name (e.g., "English", "French")
    pub name: &'static str,
    /// Flag emoji
    pub flag: &'static str,
}

/// Languages available as translation source.
/// Includes all languages since input encoding is handled by the translation API.
pub fn source_languages() -> Vec<LanguageOption> {
    vec![
        LanguageOption { code: "fr", name: "French", flag: "🇫🇷" },
        LanguageOption { code: "en", name: "English", flag: "🇬🇧" },
        LanguageOption { code: "de", name: "German", flag: "🇩🇪" },
        LanguageOption { code: "es", name: "Spanish", flag: "🇪🇸" },
        LanguageOption { code: "it", name: "Italian", flag: "🇮🇹" },
        LanguageOption { code: "pt", name: "Portuguese", flag: "🇵🇹" },
        LanguageOption { code: "zh-CN", name: "Chinese", flag: "🇨🇳" },
        LanguageOption { code: "ja", name: "Japanese", flag: "🇯🇵" },
        LanguageOption { code: "auto", name: "Auto", flag: "🔍" },
    ]
}

/// Languages available as translation target.
/// Limited to Latin-script languages due to PDF font encoding constraints
/// (WinAnsiEncoding only supports ~256 Latin characters).
pub fn target_languages() -> Vec<LanguageOption> {
    vec![
        LanguageOption { code: "en", name: "English", flag: "🇬🇧" },
        LanguageOption { code: "fr", name: "French", flag: "🇫🇷" },
        LanguageOption { code: "de", name: "German", flag: "🇩🇪" },
        LanguageOption { code: "es", name: "Spanish", flag: "🇪🇸" },
        LanguageOption { code: "it", name: "Italian", flag: "🇮🇹" },
        LanguageOption { code: "pt", name: "Portuguese", flag: "🇵🇹" },
    ]
}

/// Default source language code
pub const DEFAULT_SOURCE_LANG: &str = "fr";
/// Default target language code
pub const DEFAULT_TARGET_LANG: &str = "en";
/// Default text color name
pub const DEFAULT_TEXT_COLOR: &str = "blue";

/// Get flag emoji for a language code.
///
/// Returns a globe emoji for unknown language codes.
pub fn flag_for_lang(code: &str) -> &'static str {
    match code {
        "fr" => "🇫🇷",
        "en" => "🇬🇧",
        "de" => "🇩🇪",
        "es" => "🇪🇸",
        "it" => "🇮🇹",
        "pt" => "🇵🇹",
        "zh-CN" => "🇨🇳",
        "ja" => "🇯🇵",
        "auto" => "🔍",
        _ => "🌐",
    }
}

/// Color options for UI
pub fn color_options() -> Vec<(&'static str, TextColor)> {
    vec![
        ("Dark Red", TextColor::dark_red()),
        ("Black", TextColor::black()),
        ("Blue", TextColor::blue()),
        ("Dark Green", TextColor::dark_green()),
        ("Purple", TextColor::purple()),
    ]
}
