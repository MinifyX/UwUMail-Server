pub mod account;
pub mod admin;
pub mod auth;

use axum::extract::Path;
use axum::response::{IntoResponse, Response};

use crate::assets;
use crate::error::ApiError;

pub async fn not_found() -> ApiError {
    ApiError::NotFound("this API endpoint".into())
}

/// Every page of the app is the same HTML file; the app picks the page from the URL.
pub async fn app_page() -> Response {
    match assets::index() {
        Some(index) => assets::respond(index),
        None => ApiError::NotFound("the web app".into()).into_response(),
    }
}

pub async fn asset(Path(path): Path<String>) -> Response {
    match assets::find(&format!("/assets/{path}")) {
        Some(asset) => assets::respond(asset),
        None => ApiError::NotFound("this file".into()).into_response(),
    }
}
