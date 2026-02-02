//! Utility functions shared across the crate.

use std::path::PathBuf;

/// Get the user's config directory following XDG conventions.
///
/// Returns `$XDG_CONFIG_HOME` if set, otherwise `$HOME/.config`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

/// Get the user's cache directory following XDG conventions.
///
/// Returns `$XDG_CACHE_HOME` if set, otherwise `$HOME/.cache`.
pub fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
}

/// Get the default translation cache path.
pub fn translation_cache_path() -> PathBuf {
    cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("pdf-translator")
}

/// Clear the translation cache on disk.
///
/// Returns the number of entries cleared, or an error message.
pub fn clear_translation_cache() -> Result<usize, String> {
    let cache_path = translation_cache_path();

    if !cache_path.exists() {
        return Ok(0);
    }

    // Open the sled database and clear it
    let db = sled::open(&cache_path)
        .map_err(|e| format!("Failed to open cache: {e}"))?;

    let count = db.len();
    db.clear().map_err(|e| format!("Failed to clear cache: {e}"))?;
    db.flush().map_err(|e| format!("Failed to flush cache: {e}"))?;

    Ok(count)
}
