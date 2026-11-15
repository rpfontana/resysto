//! The HTTP application

mod config;

use std::future;
use std::path::Path;

use anyhow::Context;
use axum::Router;
use axum::extract::FromRef;
use axum::http::{HeaderValue, Response, header};
use resysto_http::AppError;
use resysto_surrealdb::Db;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::fs::ServeFileSystemResponseBody;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::{SetResponseHeader, SetResponseHeaderLayer};

/// A bundle's name carries its content hash, so the same URL never changes
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Revalidated on every load, so a deploy shows up at once
const NO_CACHE: &str = "no-cache";
/// The only part of the policy a `<meta>` cannot carry; the rest is in 200.html
const FRAME_ANCESTORS: &str = "frame-ancestors 'none'";

use crate::config::Config;

/// What the handlers can reach; each extractor takes its part through `FromRef`
#[derive(Clone, FromRef)]
struct AppState {
    db: Db,
}

/// Serves the application until Ctrl+C or SIGTERM, letting open requests finish
pub async fn serve() -> anyhow::Result<()> {
    let Config {
        addr,
        site,
        db,
        auth,
    } = resysto_config::load()?;
    let db = Db::connect(resysto_surrealdb::Config {
        endpoint: db.endpoint,
        namespace: db.namespace,
        username: db.app_user,
        password: db.app_password,
        signing_key: auth.signing_key,
        verifying_key: auth.verifying_key,
    })
    .await
    .context("connecting to SurrealDB")?;
    let state = AppState { db };
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, router(state, &site))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn router(state: AppState, site_dir: &str) -> Router {
    Router::new()
        .nest("/t/{slug}/api/v1", api())
        .with_state(state)
        .merge(site(site_dir))
}

/// The SPA's build: files as they are, `200.html` for every other path
fn site(dir: &str) -> Router {
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

// without its own fallback an unknown API path would get the SPA's page;
// tenant data must stay out of browser and proxy caches
fn api() -> Router<AppState> {
    Router::new()
        .nest("/organizations", resysto_org::organization::router())
        .fallback(async || AppError::NotFound)
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

// a handler that cannot be installed never fires, instead of shutting the server down
async fn shutdown_signal() {
    let ctrl_c = async {
        if signal::ctrl_c().await.is_err() {
            future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(_) => future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn each_path_gets_its_status_and_cache_policy() {
        let dir = env::temp_dir().join(format!("resysto-site-{}", process::id()));
        fs::create_dir_all(dir.join("_app/immutable")).unwrap();
        for file in [
            "200.html",
            "logo.svg",
            "_app/version.json",
            "_app/immutable/app.js",
        ] {
            fs::write(dir.join(file), "").unwrap();
        }
        let site = site(dir.to_str().unwrap());

        // path, status, cache-control, framing forbidden
        let cases = [
            ("/", StatusCode::OK, Some(NO_CACHE), true),
            ("/t/dev/organizations", StatusCode::OK, Some(NO_CACHE), true),
            ("/logo.svg", StatusCode::OK, Some(NO_CACHE), true),
            ("/_app/version.json", StatusCode::OK, Some(NO_CACHE), false),
            (
                "/_app/immutable/app.js",
                StatusCode::OK,
                Some(IMMUTABLE),
                false,
            ),
            (
                "/_app/immutable/gone.js",
                StatusCode::NOT_FOUND,
                None,
                false,
            ),
            (
                "/_app/gone.json",
                StatusCode::NOT_FOUND,
                Some(NO_CACHE),
                false,
            ),
        ];
        for (path, status, cache, no_framing) in cases {
            let request = Request::get(path).body(Body::empty()).unwrap();
            let response = site.clone().oneshot(request).await.unwrap();
            let get = |name: header::HeaderName| {
                response.headers().get(name).map(|v| v.to_str().unwrap())
            };
            assert_eq!(response.status(), status, "{path}");
            assert_eq!(get(header::CACHE_CONTROL), cache, "{path}");
            let framing = get(header::CONTENT_SECURITY_POLICY);
            assert_eq!(framing, no_framing.then_some(FRAME_ANCESTORS), "{path}");
        }
        let _ = fs::remove_dir_all(dir);
    }
}
