use anyhow::Result;
use pdf_translator_core::{
    AppConfig, DEFAULT_SOURCE_LANG, DEFAULT_TARGET_LANG, DEFAULT_TEXT_COLOR, Lang, PdfDocument,
    PdfTranslator, TextColor, TranslationCache, TranslatorConfig,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock as StdRwLock, Weak};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::page_store::{OutputBudget, OutputReservation, PageStore, StagedPage};

pub const MAX_SESSIONS: usize = 16;
pub const MAX_RETAINED_SOURCE_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_RETAINED_TRANSLATED_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CreateSessionError {
    #[error("session limit reached")]
    SessionLimit,
    #[error("retained source PDF storage limit exceeded")]
    StorageLimit,
    #[error("failed to create page store: {0}")]
    PageStore(#[source] std::io::Error),
}

#[derive(Debug, Error)]
pub enum CommitPageError {
    #[error("translated output storage limit exceeded")]
    StorageLimit,
    #[error("failed to publish translated page: {0}")]
    Publish(#[source] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct PageClaim(Arc<ClaimLease>);

#[derive(Debug)]
struct ClaimLease {
    page: usize,
    generation: u64,
}

impl PageClaim {
    pub fn page(&self) -> usize {
        self.0.page
    }

    pub fn generation(&self) -> u64 {
        self.0.generation
    }
}

pub struct Session {
    pub document: PdfDocument,
    pub original_filename: String,
    pub page_store: PageStore,
    pub settings: SessionSettings,
    pub last_activity: Instant,
    pub settings_generation: u64,
    pub translate_job: Option<Arc<TranslateJob>>,
    in_flight: HashMap<usize, Weak<ClaimLease>>,
}

impl Session {
    pub fn claim_page(&mut self, page: usize) -> Option<PageClaim> {
        if self
            .in_flight
            .get(&page)
            .is_some_and(|claim| claim.strong_count() != 0)
        {
            return None;
        }
        let claim = PageClaim(Arc::new(ClaimLease {
            page,
            generation: self.settings_generation,
        }));
        self.in_flight.insert(page, Arc::downgrade(&claim.0));
        Some(claim)
    }

    pub fn claim_is_current(&self, claim: &PageClaim) -> bool {
        self.settings_generation == claim.generation()
            && self
                .in_flight
                .get(&claim.page())
                .and_then(Weak::upgrade)
                .is_some_and(|active| Arc::ptr_eq(&active, &claim.0))
    }

    pub fn release_claim(&mut self, claim: &PageClaim) -> bool {
        if !self.claim_is_current(claim) {
            return false;
        }
        self.in_flight.remove(&claim.page());
        true
    }

    pub fn has_active_claims(&self) -> bool {
        self.in_flight
            .values()
            .any(|claim| claim.strong_count() != 0)
    }

    pub fn active_job(&self) -> bool {
        self.translate_job
            .as_ref()
            .is_some_and(|job| job.is_active())
    }

    pub fn job_is_current(&self, job: &Arc<TranslateJob>) -> bool {
        self.settings_generation == job.generation
            && self
                .translate_job
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, job))
    }

    pub fn invalidate_translations(&mut self) {
        self.settings_generation = self.settings_generation.wrapping_add(1);
        self.page_store.clear();
        self.in_flight.clear();
        if let Some(job) = self.translate_job.take() {
            job.set_error("Translation cancelled because settings changed".to_string());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TranslateJobState {
    Running = 0,
    Succeeded = 1,
    Failed = 2,
    Cancelled = 3,
}

pub struct TranslateJob {
    pub generation: u64,
    pub current: AtomicUsize,
    state: AtomicU8,
    error: StdRwLock<Option<String>>,
}

impl TranslateJob {
    pub const fn new(generation: u64) -> Self {
        Self {
            generation,
            current: AtomicUsize::new(0),
            state: AtomicU8::new(TranslateJobState::Running as u8),
            error: StdRwLock::new(None),
        }
    }

    pub fn state(&self) -> TranslateJobState {
        match self.state.load(Ordering::SeqCst) {
            0 => TranslateJobState::Running,
            1 => TranslateJobState::Succeeded,
            2 => TranslateJobState::Failed,
            _ => TranslateJobState::Cancelled,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state() == TranslateJobState::Running
    }

    pub fn is_done(&self) -> bool {
        !self.is_active()
    }

    pub fn increment(&self) {
        self.current.fetch_add(1, Ordering::SeqCst);
    }

    pub fn mark_succeeded(&self) {
        let _ = self.state.compare_exchange(
            TranslateJobState::Running as u8,
            TranslateJobState::Succeeded as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn cancel(&self) {
        let _ = self.state.compare_exchange(
            TranslateJobState::Running as u8,
            TranslateJobState::Cancelled as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn set_error(&self, error: String) {
        *self
            .error
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
        let _ = self.state.compare_exchange(
            TranslateJobState::Running as u8,
            TranslateJobState::Failed as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn get_error(&self) -> Option<String> {
        self.error
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Both,
    TranslatedOnly,
}

impl ViewMode {
    pub const fn viewer_class(self) -> &'static str {
        match self {
            Self::Both => "viewer",
            Self::TranslatedOnly => "viewer single",
        }
    }

    pub const fn is_translated_only(self) -> bool {
        matches!(self, Self::TranslatedOnly)
    }
}

#[derive(Clone, PartialEq)]
pub struct SessionSettings {
    pub source_lang: Lang,
    pub target_lang: Lang,
    pub text_color: TextColor,
    pub view_mode: ViewMode,
    pub auto_translate: bool,
}

impl SessionSettings {
    pub fn current_source(&self) -> &str {
        self.source_lang.as_str()
    }

    pub fn current_target(&self) -> &str {
        self.target_lang.as_str()
    }

    pub fn current_color(&self) -> &'static str {
        if self.text_color == TextColor::black() {
            "black"
        } else if self.text_color == TextColor::blue() {
            "blue"
        } else if self.text_color == TextColor::dark_green() {
            "darkgreen"
        } else if self.text_color == TextColor::purple() {
            "purple"
        } else {
            "darkred"
        }
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            source_lang: Lang::new(DEFAULT_SOURCE_LANG),
            target_lang: Lang::new(DEFAULT_TARGET_LANG),
            text_color: TextColor::from_name(DEFAULT_TEXT_COLOR).unwrap_or_default(),
            view_mode: ViewMode::default(),
            auto_translate: false,
        }
    }
}

pub struct AppState {
    sessions: RwLock<HashMap<Uuid, Session>>,
    output_budget: Arc<OutputBudget>,
    pub config: AppConfig,
    cache: TranslationCache,
}

impl AppState {
    pub fn new(api_base: String, api_key: Option<String>, model: String) -> Result<Self> {
        let config = AppConfig {
            translator: TranslatorConfig::new(api_base, api_key, model),
            ..Default::default()
        };
        let cache = TranslationCache::new(&config.cache)
            .map_err(|e| anyhow::anyhow!("Failed to initialize translation cache: {e}"))?;
        Ok(Self {
            sessions: RwLock::new(HashMap::new()),
            output_budget: Arc::new(OutputBudget::new(MAX_RETAINED_TRANSLATED_BYTES)),
            config,
            cache,
        })
    }

    pub async fn create_session(
        &self,
        doc: PdfDocument,
        filename: String,
    ) -> std::result::Result<String, CreateSessionError> {
        let source_bytes = doc.bytes().len();
        let mut sessions = self.sessions.write().await;
        if sessions.len() >= MAX_SESSIONS {
            return Err(CreateSessionError::SessionLimit);
        }
        let retained = sessions
            .values()
            .try_fold(0usize, |sum, session| {
                sum.checked_add(session.document.bytes().len())
            })
            .unwrap_or(usize::MAX);
        if retained
            .checked_add(source_bytes)
            .is_none_or(|total| total > MAX_RETAINED_SOURCE_BYTES)
        {
            return Err(CreateSessionError::StorageLimit);
        }

        let page_store = PageStore::new().map_err(CreateSessionError::PageStore)?;
        let id = Uuid::new_v4();
        let now = Instant::now();
        sessions.insert(
            id,
            Session {
                document: doc,
                original_filename: filename,
                page_store,
                settings: SessionSettings::default(),
                last_activity: now,
                settings_generation: 0,
                translate_job: None,
                in_flight: HashMap::new(),
            },
        );
        drop(sessions);
        Ok(id.to_string())
    }

    pub(crate) fn reserve_output(
        &self,
        bytes: usize,
    ) -> std::result::Result<OutputReservation, CommitPageError> {
        self.output_budget
            .reserve(bytes)
            .ok_or(CommitPageError::StorageLimit)
    }

    pub async fn get_session(&self, id: &str) -> Option<SessionRef<'_>> {
        let uuid = Uuid::parse_str(id).ok()?;
        let mut sessions = self.sessions.write().await;
        sessions.get_mut(&uuid)?.last_activity = Instant::now();
        drop(sessions);
        Some(SessionRef {
            id: uuid,
            state: self,
        })
    }

    pub fn create_translator(&self, settings: &SessionSettings) -> Result<PdfTranslator> {
        let mut config = self.config.clone();
        config.source_lang = settings.source_lang.clone();
        config.target_lang = settings.target_lang.clone();
        config.text_color = settings.text_color;
        PdfTranslator::with_cache(config, self.cache.clone())
            .map_err(|e| anyhow::anyhow!("Failed to create translator: {e}"))
    }

    pub async fn cleanup_old_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();
        let max_age = Duration::from_secs(3600);
        sessions.retain(|_, session| {
            now.duration_since(session.last_activity) < max_age
                || session.active_job()
                || session.has_active_claims()
        });
    }
}

pub struct SessionRef<'a> {
    id: Uuid,
    state: &'a AppState,
}

impl SessionRef<'_> {
    /// Commit a claim while holding the application-wide session lock.
    ///
    /// The validator, claim check, durable publication, current-page
    /// replacement, and claim release are one state transition.
    pub(crate) async fn commit_claimed_page<F>(
        &self,
        claim: &PageClaim,
        staged: StagedPage,
        can_commit: F,
    ) -> std::result::Result<Option<u64>, CommitPageError>
    where
        F: FnOnce(&Session) -> bool,
    {
        let mut sessions = self.state.sessions.write().await;
        let Some(session) = sessions.get_mut(&self.id) else {
            return Ok(None);
        };
        if !session.claim_is_current(claim) || !can_commit(session) {
            session.release_claim(claim);
            return Ok(None);
        }

        let result = (|| {
            let published = session
                .page_store
                .publish_staged(claim.page(), staged)
                .map_err(CommitPageError::Publish)?;
            let version = published.version();
            session
                .page_store
                .mark_published(published)
                .map_err(CommitPageError::Publish)?;
            Ok(Some(version))
        })();
        session.release_claim(claim);
        drop(sessions);
        result
    }

    pub async fn with_session<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&Session) -> R,
    {
        let sessions = self.state.sessions.read().await;
        sessions.get(&self.id).map(f)
    }

    pub async fn with_session_mut<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut Session) -> R,
    {
        let mut sessions = self.state.sessions.write().await;
        sessions.get_mut(&self.id).map(f)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_document() -> PdfDocument {
        PdfDocument::from_bytes(
            include_bytes!("../../pdf-translator-core/tests/fixtures/test.pdf").to_vec(),
        )
        .unwrap()
    }

    fn test_session() -> Session {
        Session {
            document: test_document(),
            original_filename: "test.pdf".to_string(),
            page_store: PageStore::new().unwrap(),
            settings: SessionSettings::default(),
            last_activity: Instant::now(),
            settings_generation: 0,
            translate_job: None,
            in_flight: HashMap::new(),
        }
    }

    fn test_state() -> AppState {
        let config = AppConfig {
            cache: pdf_translator_core::config::CacheConfig {
                memory_enabled: true,
                disk_enabled: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let cache = TranslationCache::new(&config.cache).unwrap();
        AppState {
            sessions: RwLock::new(HashMap::new()),
            output_budget: Arc::new(OutputBudget::new(MAX_RETAINED_TRANSLATED_BYTES)),
            config,
            cache,
        }
    }

    #[test]
    fn dropping_a_page_claim_unblocks_cancelled_work() {
        let mut session = test_session();
        let claim = session.claim_page(0).unwrap();
        assert!(session.has_active_claims());
        assert!(session.claim_page(0).is_none());

        drop(claim);
        assert!(!session.has_active_claims());
        assert!(session.claim_page(0).is_some());
    }

    #[test]
    fn settings_generation_rejects_stale_claims() {
        let mut session = test_session();
        let stale = session.claim_page(0).unwrap();
        assert!(session.claim_is_current(&stale));

        session.invalidate_translations();
        assert!(!session.claim_is_current(&stale));
        assert!(!session.release_claim(&stale));
        assert!(session.claim_page(0).is_some());
    }

    #[tokio::test]
    async fn session_count_limit_is_enforced_atomically() {
        let state = test_state();
        let document = test_document();
        for index in 0..MAX_SESSIONS {
            state
                .create_session(document.clone(), format!("{index}.pdf"))
                .await
                .unwrap();
        }

        assert!(matches!(
            state
                .create_session(document, "overflow.pdf".to_string())
                .await,
            Err(CreateSessionError::SessionLimit)
        ));
    }
}
