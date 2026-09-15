//! Errors of the JSON API. The app shows its own text for each `code`.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use uwumail_store::StoreError;

#[derive(Debug)]
pub enum ApiError {
    NotLoggedIn,
    /// Logged in, but not allowed (e.g. not an admin).
    Forbidden,
    /// A state-changing request without the session's CSRF token.
    CsrfMismatch,
    InvalidCredentials,
    TooManyAttempts,
    NotFound(String),
    Invalid(String),
    Conflict(String),
    /// A rule of the data model, with a stable code the app knows (e.g. `lastAdmin`).
    Rule(&'static str, String),
    Internal,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            ApiError::NotLoggedIn => (StatusCode::UNAUTHORIZED, "notLoggedIn", "Please log in.".into()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "Only admins may do this.".into()),
            ApiError::CsrfMismatch => {
                (StatusCode::FORBIDDEN, "csrfMismatch", "The request did not come from this app.".into())
            }
            ApiError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "invalidCredentials", "The address or password is wrong.".into())
            }
            ApiError::TooManyAttempts => {
                (StatusCode::TOO_MANY_REQUESTS, "tooManyAttempts", "Too many failed logins, try again later.".into())
            }
            ApiError::NotFound(what) => (StatusCode::NOT_FOUND, "notFound", format!("Not found: {what}")),
            ApiError::Invalid(detail) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid", detail.clone()),
            ApiError::Conflict(detail) => (StatusCode::CONFLICT, "conflict", detail.clone()),
            ApiError::Rule(code, detail) => (StatusCode::CONFLICT, code, detail.clone()),
            ApiError::Internal => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", "Something went wrong on the server.".into())
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, detail) = self.parts();
        let mut response = (status, Json(json!({ "code": code, "detail": detail }))).into_response();
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound(what) => ApiError::NotFound(what),
            StoreError::Invalid(detail) => ApiError::Invalid(detail),
            StoreError::Conflict(what) => ApiError::Conflict(format!("already exists: {what}")),
            StoreError::Rule { code, message } => ApiError::Rule(code, message),
            other => {
                tracing::error!(error = %other, "web API request failed");
                ApiError::Internal
            }
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
