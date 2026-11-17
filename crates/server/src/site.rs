use std::path::Path;

use axum::Router;
use axum::http::{HeaderValue, Response, header};
use tower_http::services::fs::ServeFileSystemResponseBody;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeader;

/// A bundle's name carries its content hash, so the same URL never changes
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Revalidated on every load, so a deploy shows up at once
const NO_CACHE: &str = "no-cache";
/// The only part of the policy a `<meta>` cannot carry; the rest is in 200.html
const FRAME_ANCESTORS: &str = "frame-ancestors 'none'";

/// The SPA's build: files as they are, `200.html` for every other path
pub(crate) fn router(dir: &str) -> Router {
    let bundles = SetResponseHeader::overriding(
        files(format!("{dir}/_app/immutable")),
        header::CACHE_CONTROL,
        immutable_if_found,
    );
    // version.json lives here: the client reads it to notice a deploy
    let app = SetResponseHeader::overriding(
        files(format!("{dir}/_app")),
        header::CACHE_CONTROL,
        HeaderValue::from_static(NO_CACHE),
    );
    let index = ServeFile::new(format!("{dir}/200.html"))
        .precompressed_br()
        .precompressed_gzip();
    let pages = SetResponseHeader::overriding(
        files(dir).fallback(index),
        header::CACHE_CONTROL,
        HeaderValue::from_static(NO_CACHE),
    );
    let pages = SetResponseHeader::overriding(
        pages,
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(FRAME_ANCESTORS),
    );
    // a file missing under /_app is a 404, not the SPA's page with 200: after a deploy an
    // old tab asking for a removed bundle must fail clearly
    Router::new()
        .nest_service("/_app/immutable", bundles)
        .nest_service("/_app", app)
        .fallback_service(pages)
}

fn files(dir: impl AsRef<Path>) -> ServeDir {
    ServeDir::new(dir).precompressed_br().precompressed_gzip()
}

// a cached 404 would outlive the deploy that brings the file
fn immutable_if_found(response: &Response<ServeFileSystemResponseBody>) -> Option<HeaderValue> {
    response
        .status()
        .is_success()
        .then(|| HeaderValue::from_static(IMMUTABLE))
}
