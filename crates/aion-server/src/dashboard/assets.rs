//! Static dashboard bundle serving.
//!
//! The embedded bundle is gated behind the `embed-dashboard` cargo feature.
//!
//! * With `embed-dashboard` ON, [`rust_embed`] compiles the real built dashboard
//!   from `dashboard-embed/` into the binary and [`DashboardAssetSource::Embedded`]
//!   serves it with SPA fallback. Release builds (via the aion-cli `release`
//!   feature) turn this on so the single binary ships the real UI.
//! * With `embed-dashboard` OFF (the default — keeps backend-only dev builds
//!   bun-free), [`DashboardAssetSource::Embedded`] serves a clear, branded
//!   placeholder page that documents how to run/build the dashboard. It NEVER
//!   serves a blank page or a silent stub.
//!
//! [`DashboardAssetSource::Embedded`]: crate::config::DashboardAssetSource::Embedded

use std::{borrow::Cow, path::PathBuf};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::fs;

use crate::{
    config::{DashboardAssetSource, DashboardConfig},
    error::ServerError,
};

#[derive(Clone)]
enum DashboardAssets {
    FileSystem { root: PathBuf },
    /// The compile-time embedded bundle. Only constructible when the
    /// `embed-dashboard` feature is enabled.
    #[cfg(feature = "embed-dashboard")]
    Embedded,
    /// A branded placeholder served when the binary was built WITHOUT
    /// `embed-dashboard`. It documents the dev-server URL and build command so
    /// `/` is always a useful page, never a blank or stub view.
    #[cfg(not(feature = "embed-dashboard"))]
    Placeholder,
}

/// The real dashboard bundle, embedded only when `embed-dashboard` is enabled.
///
/// The build pipeline (`cargo xtask build-dashboard`) populates `dashboard-embed/`
/// with the Vite output before the embed build runs; the committed
/// `dashboard-embed/index.html` is only a placeholder for the feature-off case.
#[cfg(feature = "embed-dashboard")]
#[derive(rust_embed::RustEmbed)]
#[folder = "dashboard-embed"]
struct EmbeddedDashboard;

/// HTML served at every route when the binary was built without
/// `embed-dashboard`. Branded, self-describing, and never blank.
#[cfg(not(feature = "embed-dashboard"))]
const PLACEHOLDER_HTML: &str = include_str!("placeholder.html");

/// Build a router that serves the configured dashboard bundle.
///
/// The returned router contains only static asset routes and fallback behaviour;
/// callers must merge it after public API/WebSocket routes so assets cannot
/// shadow public contract endpoints.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when the configured filesystem bundle lacks
/// `index.html`, or when the `embed-dashboard` feature is on but the embedded
/// bundle was built without an index.
pub fn dashboard_router(config: &DashboardConfig) -> Result<Router, ServerError> {
    let assets = resolve_assets(&config.source)?;

    Ok(Router::new()
        .route("/", get(root_asset))
        .fallback(get(path_asset))
        .with_state(assets))
}

fn resolve_assets(source: &DashboardAssetSource) -> Result<DashboardAssets, ServerError> {
    match source {
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
            Ok(DashboardAssets::FileSystem {
                root: asset_path.clone(),
            })
        }
        #[cfg(feature = "embed-dashboard")]
        DashboardAssetSource::Embedded => {
            if EmbeddedDashboard::get("index.html").is_none() {
                return Err(ServerError::Config {
                    message: "embedded dashboard bundle must contain index.html".to_owned(),
                });
            }
            Ok(DashboardAssets::Embedded)
        }
        #[cfg(not(feature = "embed-dashboard"))]
        DashboardAssetSource::Embedded => Ok(DashboardAssets::Placeholder),
    }
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
            Some(asset) => asset_response(&assets, asset_path.as_ref(), asset),
            None => serve_asset(&assets, "index.html").await,
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_asset(assets: &DashboardAssets, path: &str) -> Response {
    read_asset(assets, path).await.map_or_else(
        index_missing_response,
        |asset| asset_response(assets, path, asset),
    )
}

async fn read_asset(assets: &DashboardAssets, path: &str) -> Option<Cow<'static, [u8]>> {
    match assets {
        DashboardAssets::FileSystem { root } => {
            let bytes = fs::read(root.join(path)).await.ok()?;
            Some(Cow::Owned(bytes))
        }
        #[cfg(feature = "embed-dashboard")]
        DashboardAssets::Embedded => Some(EmbeddedDashboard::get(path)?.data),
        // The placeholder build answers every asset path with the placeholder
        // page (the SPA fallback also lands here), so `/` and any deep link
        // render the same branded "build the dashboard" guidance.
        #[cfg(not(feature = "embed-dashboard"))]
        DashboardAssets::Placeholder => Some(Cow::Borrowed(PLACEHOLDER_HTML.as_bytes())),
    }
}

fn asset_response(assets: &DashboardAssets, path: &str, asset: Cow<'static, [u8]>) -> Response {
    let mut response = Body::from(asset.into_owned()).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type_for(assets, path));
    response
}

/// Resolve the response content type. A placeholder build always serves HTML
/// (every path returns the placeholder page), so it advertises `text/html`
/// regardless of the requested extension. A real bundle uses the per-extension
/// type so JS/CSS/etc. are served correctly.
fn content_type_for(assets: &DashboardAssets, path: &str) -> HeaderValue {
    match assets {
        #[cfg(not(feature = "embed-dashboard"))]
        DashboardAssets::Placeholder => HeaderValue::from_static("text/html; charset=utf-8"),
        _ => content_type(path),
    }
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

#[cfg(test)]
mod tests {
    use axum::body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    async fn body_text(response: Response) -> Result<String, Box<dyn std::error::Error>> {
        let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    /// The embedded source always yields a usable router (the existing server
    /// tests construct it), and `/` returns 200 with an HTML index — never a
    /// blank page or a server error.
    #[tokio::test]
    async fn embedded_source_serves_html_index() -> TestResult {
        let config = DashboardConfig {
            source: DashboardAssetSource::Embedded,
        };
        let router = dashboard_router(&config)?;
        let response = router
            .oneshot(Request::builder().uri("/").body(body::Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let text = body_text(response).await?;
        assert!(text.contains("<!doctype html>") || text.contains("<!DOCTYPE html>"));
        assert!(text.contains("<html"));
        Ok(())
    }

    /// With `embed-dashboard` OFF, every route serves the branded placeholder
    /// (so a deep link is guidance, never a blank page) with an HTML content
    /// type.
    #[cfg(not(feature = "embed-dashboard"))]
    #[tokio::test]
    async fn placeholder_build_serves_branded_guidance_on_deep_links() -> TestResult {
        let config = DashboardConfig {
            source: DashboardAssetSource::Embedded,
        };
        let router = dashboard_router(&config)?;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/workflows-view/deep/link")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(content_type.starts_with("text/html"), "got {content_type}");
        let text = body_text(response).await?;
        assert!(text.contains("Dashboard not embedded"));
        assert!(text.contains("cargo xtask build-dashboard"));
        Ok(())
    }
}
