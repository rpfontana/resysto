use std::sync::Arc;

use anyhow::Context;
use axum::extract::FromRef;
use resysto_auth::{Cipher, MasterName, Passwords, Policies, PublicUrl};
use resysto_surrealdb::{Db, MASTER_DB};

use crate::config;

/// What the handlers can reach; each extractor takes its part through `FromRef`
#[derive(Clone, FromRef)]
pub(crate) struct AppState {
    pub(crate) db: Db,
    pub(crate) master: MasterName,
    pub(crate) passwords: Passwords,
    pub(crate) cipher: Cipher,
    pub(crate) policies: Arc<Policies>,
    pub(crate) public: PublicUrl,
}

impl AppState {
    pub(crate) async fn connect(
        db: config::Db,
        auth: config::Auth,
        public_url: &str,
    ) -> anyhow::Result<Self> {
        let public = public(public_url)?;
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
            public,
        })
    }
}

// clients compare the URLs built on it character by character, so no trailing slash
fn public(raw: &str) -> anyhow::Result<PublicUrl> {
    let url = raw.trim_end_matches('/');
    let authority = url.strip_prefix("https://").or_else(|| {
        url.strip_prefix("http://")
            .filter(|authority| loopback(authority))
    });
    match authority {
        Some(authority) if !authority.is_empty() && !authority.contains(['/', '?', '#']) => {
            Ok(PublicUrl(url.into()))
        }
        _ => anyhow::bail!(
            "RESYSTO_PUBLIC_URL must be https://<host>[:port], or http://localhost[:port]: {raw:?}"
        ),
    }
}

fn loopback(authority: &str) -> bool {
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    host == "localhost" || host == "127.0.0.1"
}
