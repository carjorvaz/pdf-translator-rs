//! PDF Translator Web - Web server for translating PDF documents.

mod helpers;
mod page_store;
mod routes;
mod state;
mod templates;

use anyhow::{Context, Result};
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use axum::http::{header, HeaderValue};
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    services::ServeDir,
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use std::time::Duration;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter, prelude::*};

use state::AppState;

/// Resolve the static files directory.
///
/// Priority:
/// 1. Explicit path if provided
/// 2. ./static if it exists
/// 3. Crate's built-in static directory
fn resolve_static_dir(explicit_path: Option<&str>) -> PathBuf {
    if let Some(path) = explicit_path {
        return PathBuf::from(path);
    }

    // Try ./static first (works in development and when running from crate dir)
    let local_static = PathBuf::from("static");
    if local_static.exists() && local_static.is_dir() {
        return local_static;
    }

    // Fall back to compiled-in path (useful for cargo run)
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/static"))
}

#[derive(Parser, Debug)]
#[command(name = "pdf-translator-web")]
#[command(author, version, about = "PDF Translator Web Server", long_about = None)]
struct Args {
    /// Host to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind to
    #[arg(short, long, default_value = "3000")]
    port: u16,

    /// OpenAI API base URL
    #[arg(long, env = "OPENAI_API_BASE", default_value = "http://localhost:8080/v1")]
    api_base: String,

    /// OpenAI API key
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// Model name for OpenAI-compatible API
    #[arg(long, env = "OPENAI_MODEL", default_value = "default_model")]
    model: String,

    /// Verbose output
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Static files directory (defaults to ./static or crate's static dir)
    #[arg(long, env = "STATIC_DIR")]
    static_dir: Option<String>,

    /// Clear translation cache on startup
    #[arg(long)]
    clear_cache: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present (before parsing args so env vars are available)
    dotenvy::dotenv().ok();

    let args = Args::parse();

    // Setup logging with per-crate filtering
    // font_kit emits many "Error loading font from handle: Parse" warnings for
    // system fonts it can't parse - these are expected and noisy
    let default_level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            EnvFilter::new(format!("{default_level},font_kit=error"))
        });

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false))
        .with(filter)
        .init();

    // Clear cache if requested
    if args.clear_cache {
        match pdf_translator_core::clear_translation_cache() {
            Ok(count) => info!("Cleared {} cached translations", count),
            Err(e) => tracing::warn!("Failed to clear cache: {}", e),
        }
    }

    // Create application state (opens cache - fails fast if locked)
    let state = Arc::new(
        AppState::new(args.api_base, args.api_key, args.model)
            .context("Failed to initialize application state")?
    );

    // Spawn background task for session cleanup (runs every 5 minutes)
    let cleanup_state = Arc::clone(&state);
    tokio::spawn(async move {
        let cleanup_interval = Duration::from_secs(5 * 60); // 5 minutes
        loop {
            tokio::time::sleep(cleanup_interval).await;
            cleanup_state.cleanup_old_sessions().await;
            info!("Completed session cleanup");
        }
    });

    // Build router
    let app = Router::new()
        // Pages
        .route("/", get(routes::index))
        .route("/view/{session_id}", get(routes::view_page_redirect))
        .route("/view/{session_id}/{page}", get(routes::view_page))
        // API endpoints - HTML fragments (HTMX)
        .route("/api/upload", post(routes::upload_pdf))
        .route("/api/page-view/{session_id}/{page}", get(routes::get_page_view))
        // Query-based page view for HTMX page input (hypermedia control)
        .route("/api/page-view/{session_id}", get(routes::get_page_view_query))
        .route("/api/translate/{session_id}/{page}", post(routes::translate_page))
        .route("/api/prefetch/{session_id}/{page}", post(routes::prefetch_page))
        .route("/api/translate-all/{session_id}/start", post(routes::start_translate_all))
        .route("/api/translate-all/{session_id}/stream", get(routes::translate_all_stream))
        .route("/api/settings/{session_id}", post(routes::update_settings))
        .route("/api/auto-translate/{session_id}", post(routes::toggle_auto_translate))
        .route("/api/view-mode/{session_id}/{mode}", post(routes::set_view_mode))
        // API endpoints - binary responses
        .route("/api/page/{session_id}/{page}", get(routes::get_page_image))
        .route("/api/download/{session_id}", get(routes::download_pdf))
        // Static files with Cache-Control: no-cache (cache but always revalidate via ETag)
        .nest_service(
            "/static",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::if_not_present(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache"),
                ))
                .service(ServeDir::new(resolve_static_dir(args.static_dir.as_deref()))),
        )
        // Middleware
        // Cache-Control for HTML fragments - prevents bfcache issues with HTMX
        // (images/downloads set their own headers, so this only affects HTML)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        ))
        .layer(CompressionLayer::new()) // Gzip compression for responses
        .layer(DefaultBodyLimit::max(300 * 1024 * 1024)) // 300MB limit for uploads
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    info!("Starting server at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
