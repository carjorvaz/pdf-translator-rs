//! PDF Translator Web - Web server for translating PDF documents.

mod helpers;
mod page_store;
mod routes;
mod state;
mod templates;

use anyhow::{Context, Result};
use axum::http::{HeaderValue, header};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use clap::Parser;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer, services::ServeDir, set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use state::AppState;

pub(crate) const UPLOAD_BODY_LIMIT: usize = 64 * 1024 * 1024;

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
    /// Loopback IP address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,

    /// Port to bind to
    #[arg(short, long, default_value = "3000")]
    port: u16,

    /// OpenAI API base URL
    #[arg(
        long,
        env = "OPENAI_API_BASE",
        default_value = "http://localhost:8080/v1"
    )]
    api_base: String,

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

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!("Failed to install SIGINT handler: {error}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!("Failed to install SIGTERM handler: {error}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    info!("Shutdown signal received");
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present (before parsing args so env vars are available)
    dotenvy::dotenv().ok();

    let args = Args::parse();

    if !args.host.is_loopback() {
        anyhow::bail!("refusing to bind non-loopback address {}", args.host);
    }

    let api_key = std::env::var("OPENAI_API_KEY").ok();

    // Setup logging with per-crate filtering
    // font_kit emits many "Error loading font from handle: Parse" warnings for
    // system fonts it can't parse - these are expected and noisy
    let default_level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{default_level},font_kit=error")));

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
        AppState::new(args.api_base, api_key, args.model)
            .context("Failed to initialize application state")?,
    );

    // Spawn background task for session cleanup (runs every 5 minutes)
    let cleanup_state = Arc::downgrade(&state);
    let cleanup_task = tokio::spawn(async move {
        let cleanup_interval = Duration::from_secs(5 * 60); // 5 minutes
        loop {
            tokio::time::sleep(cleanup_interval).await;
            let Some(state) = cleanup_state.upgrade() else {
                break;
            };
            state.cleanup_old_sessions().await;
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
        .route(
            "/api/page-view/{session_id}/{page}",
            get(routes::get_page_view),
        )
        // Query-based page view for HTMX page input (hypermedia control)
        .route(
            "/api/page-view/{session_id}",
            get(routes::get_page_view_query),
        )
        .route(
            "/api/translate/{session_id}/{page}",
            post(routes::translate_page),
        )
        .route(
            "/api/prefetch/{session_id}/{page}",
            post(routes::prefetch_page),
        )
        .route(
            "/api/translate-all/{session_id}/start",
            post(routes::start_translate_all),
        )
        .route(
            "/api/translate-all/{session_id}/stream",
            get(routes::translate_all_stream),
        )
        .route("/api/settings/{session_id}", post(routes::update_settings))
        .route(
            "/api/auto-translate/{session_id}",
            post(routes::toggle_auto_translate),
        )
        .route(
            "/api/view-mode/{session_id}/{page}/{mode}",
            post(routes::set_view_mode),
        )
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
                .service(ServeDir::new(resolve_static_dir(
                    args.static_dir.as_deref(),
                ))),
        )
        // Middleware
        // Cache-Control for HTML fragments - prevents bfcache issues with HTMX
        // (images/downloads set their own headers, so this only affects HTML)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        ))
        .layer(CompressionLayer::new()) // Gzip compression for responses
        .layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::new(args.host, args.port);
    info!("Starting server at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    cleanup_task.abort();
    let _ = cleanup_task.await;
    serve_result?;

    Ok(())
}
