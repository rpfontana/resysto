use std::future;

use tokio::signal;

/// Resolves at Ctrl+C or SIGTERM
// a handler that cannot be installed never fires, instead of shutting the server down
pub(crate) async fn requested() {
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
