//! The HTTP application.

use std::future;
use std::io;

use axum::Router;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::{ServeDir, ServeFile};

/// Serves the application on `RESYSTO_ADDR` until Ctrl+C or SIGTERM, then lets open requests
/// finish. The SPA comes from `RESYSTO_SITE`.
pub async fn serve() -> io::Result<()> {
    let addr = env_or("RESYSTO_ADDR", "127.0.0.1:3000");
    let site = env_or("RESYSTO_SITE", "web/build");
    let listener = TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, router(&site))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

// A path that is not a file under `site` gets its `200.html`.
fn router(site: &str) -> Router {
    let spa = ServeDir::new(site).fallback(ServeFile::new(format!("{site}/200.html")));
    Router::new().fallback_service(spa)
}

// A handler that cannot be installed never fires, instead of shutting the server down.
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
