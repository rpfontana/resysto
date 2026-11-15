//! The HTTP application

mod config;

use std::future;

use anyhow::Context;
use axum::Router;
use axum::extract::FromRef;
use resysto_http::AppError;
use resysto_surrealdb::Db;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::{ServeDir, ServeFile};

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

fn router(state: AppState, site: &str) -> Router {
    let spa = ServeDir::new(site).fallback(ServeFile::new(format!("{site}/200.html")));
    Router::new()
        .nest("/t/{slug}/api/v1", api())
        .fallback_service(spa)
        .with_state(state)
}

// without its own fallback an unknown API path would get the SPA's page
fn api() -> Router<AppState> {
    Router::new()
        .nest("/organizations", resysto_org::organization::router())
        .fallback(async || AppError::NotFound)
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
