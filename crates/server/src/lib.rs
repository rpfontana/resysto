//! The HTTP application

mod config;
mod routes;
mod scope;
mod shutdown;
mod site;
mod state;

use std::net::SocketAddr;

use resysto_surrealdb::MASTER_DB;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::state::AppState;

pub async fn serve() -> anyhow::Result<()> {
    let Config {
        addr,
        site,
        client_ip_source,
        db,
        auth,
    } = resysto_config::load()?;

    let state = AppState::connect(db, auth).await?;

    tokio::spawn(resysto_auth::sweep_forever(
        state.db.clone(),
        MASTER_DB.to_owned(),
    ));

    let listener = TcpListener::bind(&addr).await?;

    tracing::info!("listening on http://{addr}");

    let app = routes::router(state, &site, client_ip_source);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown::requested())
    .await?;
    Ok(())
}
