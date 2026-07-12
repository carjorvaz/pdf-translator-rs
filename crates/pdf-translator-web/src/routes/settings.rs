//! Settings routes - session settings management.

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use pdf_translator_core::{Lang, TextColor, source_languages, target_languages};
use std::sync::Arc;

use super::SettingsForm;
use crate::helpers::{OptionExt, RouteResult};
use crate::state::AppState;
use crate::templates::{AutoTranslateToggleTemplate, SettingsClearedTemplate};

pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Form(update): Form<SettingsForm>,
) -> RouteResult<SettingsClearedTemplate> {
    let source = match update.source_lang {
        Some(source)
            if source_languages()
                .iter()
                .any(|language| language.code == source) =>
        {
            Some(Lang::new(source))
        }
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid source language".to_string(),
            ));
        }
        None => None,
    };
    let target = match update.target_lang {
        Some(target)
            if target_languages()
                .iter()
                .any(|language| language.code == target) =>
        {
            Some(Lang::new(target))
        }
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid target language".to_string(),
            ));
        }
        None => None,
    };
    let color = match update.text_color {
        Some(color) => Some(
            TextColor::from_name(&color)
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "Invalid text color".to_string()))?,
        ),
        None => None,
    };

    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;
    session_ref
        .with_session_mut(|session| {
            let changed = source
                .as_ref()
                .is_some_and(|value| value != &session.settings.source_lang)
                || target
                    .as_ref()
                    .is_some_and(|value| value != &session.settings.target_lang)
                || color.is_some_and(|value| value != session.settings.text_color);
            if !changed {
                return;
            }
            if let Some(value) = source {
                session.settings.source_lang = value;
            }
            if let Some(value) = target {
                session.settings.target_lang = value;
            }
            if let Some(value) = color {
                session.settings.text_color = value;
            }
            // One lock transition invalidates metadata, claims, and any old-generation job.
            session.invalidate_translations();
        })
        .await
        .or_not_found("Session not found")?;

    Ok(SettingsClearedTemplate)
}

pub async fn toggle_auto_translate(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> RouteResult<AutoTranslateToggleTemplate> {
    let session_ref = state
        .get_session(&session_id)
        .await
        .or_not_found("Session not found")?;
    let new_value = session_ref
        .with_session_mut(|session| {
            session.settings.auto_translate = !session.settings.auto_translate;
            session.settings.auto_translate
        })
        .await
        .or_not_found("Session not found")?;
    Ok(AutoTranslateToggleTemplate {
        session_id,
        auto_translate: new_value,
    })
}
