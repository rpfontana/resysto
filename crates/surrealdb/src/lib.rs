//! SurrealDB access: the system connection, tenant provisioning and per-request record sessions.

mod schema;

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Namespace;
use surrealdb::types::{RecordId, ToSql};

const ACCESS: &str = "app";
const TOKEN_TTL_SECS: u64 = 15 * 60;

/// Connection settings.
pub struct Config {
    /// Server endpoint, such as `ws://127.0.0.1:18000`.
    pub endpoint: String,
    /// Namespace holding the master and tenant databases.
    pub namespace: String,
    /// Namespace user with the `OWNER` role.
    pub username: String,
    /// Password of `username`.
    pub password: String,
    /// PKCS#8 PEM of the ES256 key that signs record session tokens.
    pub signing_key: String,
    /// SPKI PEM of the matching public key, stored in each database's `app` access.
    pub verifying_key: String,
}

/// A connection signed in as the namespace user.
pub struct Db {
    system: Surreal<Any>,
    namespace: String,
    signing_key: EncodingKey,
    verifying_key: String,
}

/// Errors returned by [`Db`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database rejected the request.
    #[error(transparent)]
    Db(#[from] surrealdb::Error),
    /// The signing key is invalid.
    #[error(transparent)]
    Token(#[from] jsonwebtoken::errors::Error),
    /// A database name with characters outside `[a-z0-9_]`.
    #[error("invalid database name {0:?}")]
    InvalidName(String),
}

#[derive(Serialize)]
struct Claims<'a> {
    #[serde(rename = "NS")]
    ns: &'a str,
    #[serde(rename = "DB")]
    db: &'a str,
    #[serde(rename = "AC")]
    ac: &'a str,
    #[serde(rename = "ID")]
    id: String,
    iat: u64,
    exp: u64,
}

impl Db {
    /// Connects and signs in as the namespace user.
    pub async fn connect(config: Config) -> Result<Self, Error> {
        let system = any::connect(config.endpoint).await?;
        system
            .signin(Namespace {
                namespace: config.namespace.clone(),
                username: config.username,
                password: config.password,
            })
            .await?;
        system.use_ns(&config.namespace).await?;
        Ok(Self {
            system,
            namespace: config.namespace,
            signing_key: EncodingKey::from_ec_pem(config.signing_key.as_bytes())?,
            verifying_key: config.verifying_key,
        })
    }

    async fn system_session(&self, database: &str) -> Result<Surreal<Any>, Error> {
        let session: Surreal<Any> = self.system.clone();
        session.use_db(database).await?;
        Ok(session)
    }

    /// A new record session on `database` as `user`, through the database's `app` access.
    pub async fn record_session(
        &self,
        database: &str,
        user: impl Into<RecordId>,
    ) -> Result<Surreal<Any>, Error> {
        let token = self.record_token(database, &user.into())?;
        let session = self.system.clone();
        session.authenticate(token).await?;
        Ok(session)
    }

    fn record_token(&self, database: &str, user: &RecordId) -> Result<String, Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let claims = Claims {
            ns: &self.namespace,
            db: database,
            ac: ACCESS,
            id: user.to_sql(),
            iat: now,
            exp: now + TOKEN_TTL_SECS,
        };
        Ok(jsonwebtoken::encode(
            &Header::new(Algorithm::ES256),
            &claims,
            &self.signing_key,
        )?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use surrealdb::types::{Uuid, Value};

    use super::*;

    // Test-only key pair.
    const SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgCcu34KTFnm0J5qWJ
AxhAcHSbqFj+B/am0htHlDufqWihRANCAASN/BWdDI7GaVVD6HAh20C4ZzJSHSsz
9htiM2MVKHWJFnxAy4GKb6YFXGV6KEDsrn5MQIa7TTsyRDnIcCjQztXz
-----END PRIVATE KEY-----
";
    const VERIFYING_KEY: &str = "-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEjfwVnQyOxmlVQ+hwIdtAuGcyUh0r
M/YbYjNjFSh1iRZ8QMuBim+mBVxleihA7K5+TECGu007MkQ5yHAo0M7V8w==
-----END PUBLIC KEY-----
";
    const OTHER_SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgS6SEohQblo3QRmA0
8eCrKQUp2R6yas6xs0mNHo9bfV2hRANCAARX+qrKRM8k5KAXynI8PN0JT+7d+0w/
yHO+6eCX0n3GmE1BdcTih8Sc/MyegLajWTy5dEDEGDhjWNmkXi5uMc+s
-----END PRIVATE KEY-----
";

    pub(crate) async fn connect() -> Db {
        connect_signing_with(SIGNING_KEY).await
    }

    async fn connect_signing_with(signing_key: &str) -> Db {
        let port = std::env::var("SURREAL_PORT").unwrap_or_else(|_| "18000".into());
        Db::connect(Config {
            endpoint: format!("ws://127.0.0.1:{port}"),
            namespace: "resysto-dev".into(),
            username: "ns_migrator".into(),
            password: "migrator".into(),
            signing_key: signing_key.into(),
            verifying_key: VERIFYING_KEY.into(),
        })
        .await
        .unwrap()
    }

    pub(crate) async fn new_tenant(db: &Db) -> String {
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        db.provision_tenant(&name).await.unwrap();
        name
    }

    pub(crate) async fn info(db: &Db, database: &str) -> Value {
        let session = db.system_session(database).await.unwrap();
        session.query("INFO FOR DB").await.unwrap().take(0).unwrap()
    }

    async fn whoami(session: &Surreal<Any>) -> (Option<RecordId>, Option<String>) {
        let mut response = session
            .query("RETURN $auth; RETURN session::db();")
            .await
            .unwrap();
        (response.take(0).unwrap(), response.take(1).unwrap())
    }

    #[tokio::test]
    async fn record_sessions_are_separate_and_leave_the_system_session_alone() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let alice = RecordId::new("account", Uuid::new_v7());
        let bob = RecordId::new("account", Uuid::new_v7());

        let as_alice = db.record_session(&tenant, alice.clone()).await.unwrap();
        let as_bob = db.record_session(&tenant, bob.clone()).await.unwrap();

        assert_eq!(whoami(&as_alice).await, (Some(alice), Some(tenant.clone())));
        assert_eq!(whoami(&as_bob).await, (Some(bob), Some(tenant.clone())));
        // A record user can't read INFO, so this fails if a record session took over the system one.
        info(&db, &tenant).await;
    }

    #[tokio::test]
    async fn a_token_signed_with_another_key_is_rejected() {
        let tenant = new_tenant(&connect().await).await;
        let forger = connect_signing_with(OTHER_SIGNING_KEY).await;
        let user = RecordId::new("account", Uuid::new_v7());
        assert!(forger.record_session(&tenant, user).await.is_err());
    }

    #[tokio::test]
    async fn a_record_session_cannot_reach_another_tenant() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let other = new_tenant(&db).await;
        db.system_session(&other)
            .await
            .unwrap()
            .query("DEFINE TABLE secret PERMISSIONS FULL; CREATE secret:1;")
            .await
            .unwrap()
            .check()
            .unwrap();

        let session = db
            .record_session(&tenant, RecordId::new("account", Uuid::new_v7()))
            .await
            .unwrap();

        assert!(session.use_db(&other).await.is_err(), "switched to {other}");
        let rows: Vec<Value> = session
            .query("SELECT * FROM secret")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(rows.is_empty(), "read {rows:?} from another tenant");
    }
}
