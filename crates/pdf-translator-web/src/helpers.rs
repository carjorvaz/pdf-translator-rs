//! Helper types and traits for cleaner route handlers.
//!
//! Provides extension traits for converting `Option` and `Result` types
//! into HTTP-appropriate error responses, reducing boilerplate in routes.

use axum::http::StatusCode;

/// Standard result type for route handlers returning HTML.
pub type RouteResult<T> = Result<T, (StatusCode, String)>;

/// Extension trait for converting `Option<T>` to `RouteResult<T>`.
///
/// Provides convenient methods for returning 404 Not Found when
/// an expected resource (like a session) doesn't exist.
pub trait OptionExt<T> {
    /// Returns the contained value or a 404 Not Found error.
    fn or_not_found(self, msg: &str) -> RouteResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn or_not_found(self, msg: &str) -> RouteResult<T> {
        self.ok_or_else(|| (StatusCode::NOT_FOUND, msg.to_string()))
    }
}

/// Extension trait for converting `Result<T, E>` to `RouteResult<T>`.
///
/// Provides convenient methods for converting errors into
/// appropriate HTTP status codes.
pub trait ResultExt<T, E: std::fmt::Display> {
    /// Converts the error to 500 Internal Server Error.
    fn or_internal_error(self) -> RouteResult<T>;

    /// Converts the error to 400 Bad Request.
    fn or_bad_request(self) -> RouteResult<T>;
}

impl<T, E: std::fmt::Display> ResultExt<T, E> for Result<T, E> {
    fn or_internal_error(self) -> RouteResult<T> {
        self.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    }

    fn or_bad_request(self) -> RouteResult<T> {
        self.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
    }
}

/// Validate that a page number is within bounds.
///
/// Returns 400 Bad Request if page >= page_count.
pub fn validate_page(page: usize, page_count: usize) -> RouteResult<()> {
    if page >= page_count {
        Err((
            StatusCode::BAD_REQUEST,
            format!("Page {page} out of range (0..{page_count})"),
        ))
    } else {
        Ok(())
    }
}
