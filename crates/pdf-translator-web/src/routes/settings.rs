//! Settings routes - session settings management.

use axum::extract::{Path, State, Form};
use axum::http::StatusCode;
use pdf_translator_core::{Lang, TextColor};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::SettingsForm;
use crate::helpers::{OptionExt, RouteResult};
use crate::state::AppState;
use crate::templates::{AutoTranslateToggleTemplate, SettingsClearedTemplate};

/// Update session settings - returns cleared panel HTML fragment.
///
/// HTMX: Replaces `#translated-content`, includes OOB swaps for flag/swatch indicators.
/// This keeps all UI state server-controlled (hypermedia-style).
/// Rejects changes during active batch translation to prevent race conditions.
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Form(update): Form<SettingsForm>,
) -> RouteResult<SettingsClearedTemplate> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    // Reject settings changes during active batch translation
    let is_translating = session_ref
        .with_session(|s| {
            s.translate_job
                .as_ref()
                .is_some_and(|job| !job.done.load(Ordering::SeqCst))
        })
        .await
        .unwrap_or(false);

    if is_translating {
        return Err((
            StatusCode::CONFLICT,
            "Cannot change settings during batch translation".to_string(),
        ));
    }

    // Update settings
    session_ref
        .with_session_mut(|s| {
            if let Some(ref source) = update.source_lang {
                s.settings.source_lang = Lang::new(source.clone());
            }
            if let Some(ref target) = update.target_lang {
                s.settings.target_lang = Lang::new(target.clone());
            }
            if let Some(ref color) = update.text_color
                && let Some(c) = TextColor::from_name(color)
            {
                s.settings.text_color = c;
            }
            // Clear translated pages when settings change
            s.page_store.clear();
        })
        .await
        .or_not_found("Session not found")?;

    Ok(SettingsClearedTemplate::default())
}

/// Toggle auto-translate setting - returns OOB checkbox update.
///
/// Unlike language/color settings, this doesn't clear translations.
pub async fn toggle_auto_translate(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<AutoTranslateToggleTemplate> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;

    let new_value = session_ref
        .with_session_mut(|s| {
            s.settings.auto_translate = !s.settings.auto_translate;
            s.settings.auto_translate
        })
        .await
        .or_not_found("Session not found")?;

    Ok(AutoTranslateToggleTemplate {
        session_id,
        auto_translate: new_value,
    })
}
