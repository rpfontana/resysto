//! The HTTP application

mod settings;

use std::future;
use std::io;

use axum::Router;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::{ServeDir, ServeFile};

use crate::settings::Settings;

/// Serves the application until Ctrl+C or SIGTERM, letting open requests finish
pub async fn serve() -> io::Result<()> {
    let Settings { addr, site } = resysto_config::load().map_err(io::Error::other)?;
    let listener = TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, router(&site))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

// client-side routes have no file of their own: the SPA's fallback page serves them
fn router(site: &str) -> Router {
    let spa = ServeDir::new(site).fallback(ServeFile::new(format!("{site}/200.html")));
    Router::new().fallback_service(spa)
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
