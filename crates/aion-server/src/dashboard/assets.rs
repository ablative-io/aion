//! Static dashboard bundle serving.

use std::{borrow::Cow, path::PathBuf};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::RustEmbed;
use tokio::fs;

use crate::{
    config::{DashboardAssetSource, DashboardConfig},
    error::ServerError,
};

#[derive(Clone)]
enum DashboardAssets {
    FileSystem { root: PathBuf },
    Embedded,
}

#[derive(RustEmbed)]
#[folder = "dashboard-embed"]
struct EmbeddedDashboard;

/// Build a router that serves the configured dashboard bundle.
///
/// The returned router contains only static asset routes and fallback behaviour;
/// callers must merge it after public API/WebSocket routes so assets cannot
/// shadow public contract endpoints.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when the configured filesystem bundle lacks
/// `index.html` or when the embedded bundle was built without an index.
pub fn dashboard_router(config: &DashboardConfig) -> Result<Router, ServerError> {
    let assets = match &config.source {
        DashboardAssetSource::FileSystem { asset_path } => {
            let index_path = asset_path.join("index.html");
            if !index_path.is_file() {
                return Err(ServerError::Config {
                    message: format!(
                        "dashboard asset bundle `{}` must contain index.html",
                        asset_path.display()
                    ),
                });
            }
            DashboardAssets::FileSystem {
                root: asset_path.clone(),
            }
        }
        DashboardAssetSource::Embedded => {
            if EmbeddedDashboard::get("index.html").is_none() {
                return Err(ServerError::Config {
                    message: "embedded dashboard bundle must contain index.html".to_owned(),
                });
            }
            DashboardAssets::Embedded
        }
    };

    Ok(Router::new()
        .route("/", get(root_asset))
        .fallback(get(path_asset))
        .with_state(assets))
}

async fn root_asset(State(assets): State<DashboardAssets>) -> Response {
    serve_asset(&assets, "index.html").await
}

async fn path_asset(State(assets): State<DashboardAssets>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if is_reserved_public_path(path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match sanitize_path(path) {
        Some(asset_path) => match read_asset(&assets, &asset_path).await {
            Some(asset) => asset_response(asset_path.as_ref(), asset),
            None => serve_asset(&assets, "index.html").await,
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_asset(assets: &DashboardAssets, path: &str) -> Response {
    read_asset(assets, path)
        .await
        .map_or_else(index_missing_response, |asset| asset_response(path, asset))
}

async fn read_asset(assets: &DashboardAssets, path: &str) -> Option<Cow<'static, [u8]>> {
    match assets {
        DashboardAssets::FileSystem { root } => {
            let bytes = fs::read(root.join(path)).await.ok()?;
            Some(Cow::Owned(bytes))
        }
        DashboardAssets::Embedded => Some(EmbeddedDashboard::get(path)?.data),
    }
}

fn asset_response(path: &str, asset: Cow<'static, [u8]>) -> Response {
    let mut response = Body::from(asset.into_owned()).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type(path));
    response
}

fn sanitize_path(path: &str) -> Option<String> {
    if path.is_empty()
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return None;
    }
    Some(path.to_owned())
}

fn is_reserved_public_path(path: &str) -> bool {
    path == "workflows"
        || path.starts_with("workflows/")
        || path == "events"
        || path.starts_with("events/")
}

fn content_type(path: &str) -> HeaderValue {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str);
    if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("html")) {
        HeaderValue::from_static("text/html; charset=utf-8")
    } else if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("js")) {
        HeaderValue::from_static("text/javascript; charset=utf-8")
    } else if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("css")) {
        HeaderValue::from_static("text/css; charset=utf-8")
    } else if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
        HeaderValue::from_static("application/json")
    } else if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) {
        HeaderValue::from_static("image/svg+xml")
    } else {
        HeaderValue::from_static("application/octet-stream")
    }
}

fn index_missing_response() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "dashboard index missing").into_response()
}
