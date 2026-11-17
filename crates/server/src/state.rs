use std::sync::Arc;

use anyhow::Context;
use axum::extract::FromRef;
use resysto_auth::{Cipher, MasterName, Passwords, Policies};
use resysto_surrealdb::{Db, MASTER_DB};

use crate::config;

/// What the handlers can reach; each extractor takes its part through `FromRef`
#[derive(Clone, FromRef)]
pub(crate) struct AppState {
    pub(crate) db: Db,
    master: MasterName,
    passwords: Passwords,
    cipher: Cipher,
    policies: Arc<Policies>,
}

impl AppState {
    pub(crate) async fn connect(db: config::Db, auth: config::Auth) -> anyhow::Result<Self> {
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
        let passwords = Passwords::new(auth.password_pepper)?;
        let cipher = Cipher::new(&auth.encryption_key)?;
        let policies = Arc::new(Policies::load()?);
        Ok(Self {
            db,
            master: MasterName(MASTER_DB.into()),
            passwords,
            cipher,
            policies,
        })
    }
}
