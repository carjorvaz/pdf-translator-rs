use anyhow::Result;
use pdf_translator_core::{
    AppConfig, Lang, PdfDocument, PdfTranslator, TextColor, TranslatorConfig,
    DEFAULT_SOURCE_LANG, DEFAULT_TARGET_LANG, DEFAULT_TEXT_COLOR,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::page_store::PageStore;

/// Session data for a PDF translation session
pub struct Session {
    pub document: PdfDocument,
    pub original_filename: String,
    /// Disk-backed storage for translated pages (replaces in-memory HashMap)
    pub page_store: PageStore,
    pub settings: SessionSettings,
    pub created_at: std::time::Instant,
    /// Active translate-all job
    pub translate_job: Option<Arc<TranslateJob>>,
    /// Currently viewed page (for restoring state)
    pub current_page: usize,
}

/// Progress tracking for translate-all jobs
#[derive(Default)]
pub struct TranslateJob {
    pub current: AtomicUsize,
    pub done: AtomicBool,
    pub error: RwLock<Option<String>>,
}

impl TranslateJob {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&self) {
        self.current.fetch_add(1, Ordering::SeqCst);
    }

    pub fn mark_done(&self) {
        self.done.store(true, Ordering::SeqCst);
    }

    pub async fn set_error(&self, error: String) {
        *self.error.write().await = Some(error);
    }

    pub async fn get_error(&self) -> Option<String> {
        self.error.read().await.clone()
    }
}

/// Per-session settings
#[derive(Clone)]
pub struct SessionSettings {
    pub source_lang: Lang,
    pub target_lang: Lang,
    pub text_color: TextColor,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            source_lang: Lang::new(DEFAULT_SOURCE_LANG),
            target_lang: Lang::new(DEFAULT_TARGET_LANG),
            text_color: TextColor::from_name(DEFAULT_TEXT_COLOR).unwrap_or_default(),
        }
    }
}

/// Global application state
pub struct AppState {
    /// Active sessions indexed by UUID
    sessions: RwLock<HashMap<Uuid, Session>>,
    /// Base configuration (contains OpenAI settings in translator field)
    pub config: AppConfig,
}

impl AppState {
    pub fn new(api_base: String, api_key: Option<String>, model: String) -> Self {
        let config = AppConfig {
            translator: TranslatorConfig::new(api_base, api_key, model),
            ..Default::default()
        };

        Self {
            sessions: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Create a new session with a PDF document.
    ///
    /// Returns the session ID as a string (for URL embedding).
    /// Returns an error if the page store cannot be created.
    pub async fn create_session(&self, doc: PdfDocument, filename: String) -> Result<String> {
        let id = Uuid::new_v4();

        let page_store = PageStore::new()
            .map_err(|e| anyhow::anyhow!("Failed to create page store: {e}"))?;

        let session = Session {
            document: doc,
            original_filename: filename,
            page_store,
            settings: SessionSettings::default(),
            created_at: std::time::Instant::now(),
            translate_job: None,
            current_page: 0,
        };

        self.sessions.write().await.insert(id, session);
        Ok(id.to_string())
    }

    /// Get a session by ID string.
    ///
    /// Returns `None` if the ID is not a valid UUID or session doesn't exist.
    pub async fn get_session(&self, id: &str) -> Option<SessionRef<'_>> {
        let uuid = Uuid::parse_str(id).ok()?;
        let sessions = self.sessions.read().await;
        if sessions.contains_key(&uuid) {
            Some(SessionRef {
                id: uuid,
                state: self,
            })
        } else {
            None
        }
    }

    /// Create a translator for a session.
    pub fn create_translator(&self, settings: &SessionSettings) -> Result<PdfTranslator> {
        let mut config = self.config.clone();
        config.source_lang = settings.source_lang.clone();
        config.target_lang = settings.target_lang.clone();
        config.text_color = settings.text_color;

        PdfTranslator::new(config).map_err(|e| anyhow::anyhow!("Failed to create translator: {e}"))
    }

    /// Cleanup old sessions (older than 1 hour)
    pub async fn cleanup_old_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        let now = std::time::Instant::now();
        let max_age = std::time::Duration::from_secs(3600);

        sessions.retain(|_, session| {
            now.duration_since(session.created_at) < max_age
        });
    }
}

/// A borrowed reference to a session that provides safe access patterns.
///
/// # Why This Pattern?
///
/// In async Rust, holding a lock guard (like `RwLockReadGuard`) across an
/// `.await` point is problematic - it can cause deadlocks and the guard
/// isn't `Send`. This pattern solves that by:
///
/// 1. Storing only the session ID and a reference to the state
/// 2. Acquiring locks only within synchronous closures
/// 3. Releasing locks before any `.await` points
///
/// # Usage
///
/// ```ignore
/// // Good: Lock is released before any await
/// let (a, b) = session.with_session(|s| (s.field_a.clone(), s.field_b)).await?;
/// do_async_work(a, b).await;
///
/// // Bad (won't compile): Holding lock across await
/// let guard = sessions.read().await;
/// let session = guard.get(&id)?;
/// do_async_work(&session.field).await; // Error: guard not Send
/// ```
pub struct SessionRef<'a> {
    id: Uuid,
    state: &'a AppState,
}

impl SessionRef<'_> {
    /// Access session data immutably within a closure.
    ///
    /// The closure runs synchronously while holding a read lock.
    /// The lock is released before this method returns.
    pub async fn with_session<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&Session) -> R,
    {
        let sessions = self.state.sessions.read().await;
        sessions.get(&self.id).map(f)
    }

    /// Access session data mutably within a closure.
    ///
    /// The closure runs synchronously while holding a write lock.
    /// The lock is released before this method returns.
    pub async fn with_session_mut<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut Session) -> R,
    {
        let mut sessions = self.state.sessions.write().await;
        sessions.get_mut(&self.id).map(f)
    }
}
