//! SurrealDB access: the system connection and per-request record sessions

#[cfg(feature = "mem")]
mod mem;
mod page;
mod schema;
mod transaction;

pub use page::{Cursor, InvalidCursor, Page, PageRequest, page, write_in_batches};
pub use schema::{SchemaFile, apply_schema, changed_schema, dry_run_schema};
pub use transaction::{Executor, Tx, transaction};

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Namespace;
use surrealdb::types::{RecordId, SurrealValue, ToSql};
use tokio::sync::RwLock;

/// The database of the master lane, which registers the tenants
pub const MASTER_DB: &str = "__master";

const ACCESS: &str = "app";
const TOKEN_TTL_SECS: u64 = 60;

/// How long a namespace user's token serves new sessions before a new signin; it must stay below
/// the user's `DURATION FOR TOKEN`, 1h by default
const RENEW_AFTER: Duration = Duration::from_secs(30 * 60);

/// Connection settings
pub struct Config {
    /// Server endpoint, such as `ws://127.0.0.1:18000`
    pub endpoint: String,
    /// Namespace holding the master and tenant databases
    pub namespace: String,
    /// Namespace user with the `OWNER` role
    pub username: String,
    /// Password of `username`
    pub password: SecretString,
    /// PKCS#8 PEM of the ES256 key that signs record session tokens
    pub signing_key: SecretString,
    /// SPKI PEM of the matching public key, stored in each database's `app` access
    pub verifying_key: String,
}

/// A connection with sessions as the namespace user or as a record user; cloning it is cheap
#[derive(Clone)]
pub struct Db(Arc<Inner>);

struct Inner {
    // never authenticated: record sessions start here
    bare: Surreal<Any>,
    // authenticated with a token of the namespace user: system sessions start here
    system: RwLock<Arc<System>>,
    namespace: String,
    username: String,
    password: SecretString,
    signing_key: EncodingKey,
    verifying_key: String,
}

struct System {
    session: Surreal<Any>,
    renew_at: Instant,
}

/// What a record session reaches; the database's `PERMISSIONS` read it from the token
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    /// The organizations whose records the session sees; `None` sees the whole tenant
    pub organizations: Option<Vec<RecordId>>,
    pub write: bool,
}

/// Who a record session writes as; the audit fields and events read it from the token
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    /// The member the session runs as, which `$auth` holds
    pub member: RecordId,
    pub via: Via,
    /// The `client_id` of the agent's app, for writes through MCP
    pub client: Option<String>,
    /// The key of the browser session or of the access token the request came with
    pub session: Option<String>,
    /// The request's `x-request-id`, shared by every change it makes
    pub request: Option<String>,
    /// The address the request came from
    pub ip: Option<IpAddr>,
    /// The browser's `User-Agent`
    pub user_agent: Option<String>,
}

/// The channel a write came through
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, SurrealValue)]
#[serde(rename_all = "lowercase")]
#[surreal(untagged, rename_all = "lowercase")]
pub enum Via {
    /// The SPA, with a browser session
    Ui,
    /// An agent through MCP, with an access token
    Mcp,
}

/// Errors returned by [`Db`]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database rejected the request
    #[error(transparent)]
    Db(#[from] surrealdb::Error),
    /// The signing key is invalid
    #[error(transparent)]
    Token(#[from] jsonwebtoken::errors::Error),
    /// A database name with characters outside `[a-z0-9_]`
    #[error("invalid database name {0:?}")]
    InvalidName(String),
    /// A [`transaction`] closure kept its [`Tx`] past the end of its future
    #[error("transaction still in use after its closure finished")]
    TxInUse,
    /// The database rejected a schema file
    #[error("schema file {path} failed")]
    Schema {
        path: String,
        source: Box<surrealdb::Error>,
    },
    /// A [`page`] query returned a row without a UUID `id`
    #[error("page row without a uuid id")]
    RowWithoutId,
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
    org_all: bool,
    org_scope: Vec<String>,
    write: bool,
    via: Via,
    #[serde(skip_serializing_if = "Option::is_none")]
    client: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ua: Option<&'a str>,
}

impl Db {
    /// Connects and signs in as the namespace user
    pub async fn connect(config: Config) -> Result<Self, Error> {
        let bare = any::connect(config.endpoint.as_str()).await?;
        Self::open(bare, config).await
    }

    async fn open(bare: Surreal<Any>, config: Config) -> Result<Self, Error> {
        let system = sign_in(&bare, &config.namespace, &config.username, &config.password).await?;
        Ok(Self(Arc::new(Inner {
            bare,
            system: RwLock::new(Arc::new(system)),
            namespace: config.namespace,
            username: config.username,
            password: config.password,
            signing_key: EncodingKey::from_ec_pem(config.signing_key.expose_secret().as_bytes())?,
            verifying_key: config.verifying_key,
        })))
    }

    /// Creates `database` if missing
    pub async fn define_database(&self, database: &str) -> Result<(), Error> {
        check_name(database)?;
        self.namespace_session()
            .await?
            .query(format!("DEFINE DATABASE IF NOT EXISTS `{database}`"))
            .await?
            .check()?;
        Ok(())
    }

    /// The names of the namespace's databases
    pub async fn databases(&self) -> Result<Vec<String>, Error> {
        // without a database selected, 4.0.0 fails on INFO inside an expression
        let names: Vec<String> = self
            .namespace_session()
            .await?
            .query("LET $info = INFO FOR NS; RETURN object::keys($info.databases);")
            .await?
            .check()?
            .take(1)?;
        Ok(names)
    }

    /// A new session on `database` as the namespace user; opening it creates a missing database
    pub async fn system_session(&self, database: &str) -> Result<Surreal<Any>, Error> {
        let session = self.namespace_session().await?;
        session.use_db(database).await?;
        Ok(session)
    }

    // a new session as the namespace user on no database, signing in again once the token is old
    async fn namespace_session(&self) -> Result<Surreal<Any>, Error> {
        let current = Arc::clone(&*self.0.system.read().await);
        if Instant::now() < current.renew_at {
            return Ok(current.session.clone());
        }
        let mut system = self.0.system.write().await;
        // another request may have renewed it while this one waited for the lock
        if Instant::now() >= system.renew_at {
            *system = Arc::new(
                sign_in(
                    &self.0.bare,
                    &self.0.namespace,
                    &self.0.username,
                    &self.0.password,
                )
                .await?,
            );
        }
        Ok(system.session.clone())
    }

    /// The SPKI PEM that each database's `app` access verifies record session tokens with
    pub fn verifying_key(&self) -> &str {
        &self.0.verifying_key
    }

    /// A new record session on `database` as `actor`, through the database's `app` access
    pub async fn record_session(
        &self,
        database: &str,
        actor: &Actor,
        grant: &Grant,
    ) -> Result<Surreal<Any>, Error> {
        let token = self.record_token(database, actor, grant)?;
        let session = self.0.bare.clone();
        session.authenticate(token).await?;
        Ok(session)
    }

    fn record_token(&self, database: &str, actor: &Actor, grant: &Grant) -> Result<String, Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let claims = Claims {
            ns: &self.0.namespace,
            db: database,
            ac: ACCESS,
            id: actor.member.to_sql(),
            iat: now,
            exp: now + TOKEN_TTL_SECS,
            org_all: grant.organizations.is_none(),
            org_scope: grant
                .organizations
                .iter()
                .flatten()
                .map(ToSql::to_sql)
                .collect(),
            write: grant.write,
            via: actor.via,
            client: actor.client.as_deref(),
            sid: actor.session.as_deref(),
            rid: actor.request.as_deref(),
            ip: actor.ip,
            ua: actor.user_agent.as_deref(),
        };
        Ok(jsonwebtoken::encode(
            &Header::new(Algorithm::ES256),
            &claims,
            &self.0.signing_key,
        )?)
    }
}

// signs in on a throwaway session and hands the token to a new one, whose clones then replay the
// token and not the password
async fn sign_in(
    bare: &Surreal<Any>,
    namespace: &str,
    username: &str,
    password: &SecretString,
) -> Result<System, Error> {
    let renew_at = Instant::now() + RENEW_AFTER;
    let token = bare
        .clone()
        .signin(Namespace {
            namespace: namespace.to_owned(),
            username: username.to_owned(),
            password: password.expose_secret().to_owned(),
        })
        .await?;
    let session = bare.clone();
    session.authenticate(token).await?;
    session.use_ns(namespace).await?;
    Ok(System { session, renew_at })
}

// the name is interpolated into SurrealQL as an identifier
fn check_name(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use surrealdb::types::{Uuid, Value};

    use super::*;

    // test-only key pair
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

    // the migrator provisions real tenants; here only the `app` access is needed
    pub(crate) async fn new_tenant(db: &Db) -> String {
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        db.define_database(&name).await.unwrap();
        db.system_session(&name)
            .await
            .unwrap()
            .query(format!(
                "DEFINE ACCESS {ACCESS} ON DATABASE TYPE RECORD WITH JWT ALGORITHM ES256 KEY $key"
            ))
            .bind(("key", VERIFYING_KEY))
            .await
            .unwrap()
            .check()
            .unwrap();
        name
    }

    pub(crate) async fn info(db: &Db, database: &str) -> Value {
        let session = db.system_session(database).await.unwrap();
        session.query("INFO FOR DB").await.unwrap().take(0).unwrap()
    }

    fn everything() -> Grant {
        Grant {
            organizations: None,
            write: true,
        }
    }

    fn in_ui(member: RecordId) -> Actor {
        Actor {
            member,
            via: Via::Ui,
            client: None,
            session: None,
            request: None,
            ip: None,
            user_agent: None,
        }
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
        let alice = RecordId::new("member", Uuid::new_v7());
        let bob = RecordId::new("member", Uuid::new_v7());

        let as_alice = db
            .record_session(&tenant, &in_ui(alice.clone()), &everything())
            .await
            .unwrap();
        let as_bob = db
            .record_session(&tenant, &in_ui(bob.clone()), &everything())
            .await
            .unwrap();

        assert_eq!(whoami(&as_alice).await, (Some(alice), Some(tenant.clone())));
        assert_eq!(whoami(&as_bob).await, (Some(bob), Some(tenant.clone())));
        // a record user can't read INFO, so this fails if a record session took over the system one
        info(&db, &tenant).await;
    }

    #[tokio::test]
    async fn the_token_carries_the_claims_the_audit_reads() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let actor = Actor {
            session: Some("5e55".into()),
            request: Some("r-1".into()),
            ip: Some(IpAddr::from([10, 0, 0, 7])),
            user_agent: Some("Firefox".into()),
            ..in_ui(RecordId::new("member", Uuid::new_v7()))
        };

        let session = db
            .record_session(&tenant, &actor, &everything())
            .await
            .unwrap();

        // the names the schema files' events and fields read
        let claims: Value = session
            .query("RETURN [$token.via, $token.sid, $token.rid, $token.ip, $token.ua]")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(
            claims.to_sql(),
            "['ui', '5e55', 'r-1', '10.0.0.7', 'Firefox']"
        );
    }

    #[tokio::test]
    async fn a_token_signed_with_another_key_is_rejected() {
        let tenant = new_tenant(&connect().await).await;
        let forger = connect_signing_with(OTHER_SIGNING_KEY).await;
        let user = in_ui(RecordId::new("member", Uuid::new_v7()));
        assert!(
            forger
                .record_session(&tenant, &user, &everything())
                .await
                .is_err()
        );
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
            .record_session(
                &tenant,
                &in_ui(RecordId::new("member", Uuid::new_v7())),
                &everything(),
            )
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

    #[tokio::test]
    async fn sessions_work_again_after_the_namespace_token_expires() {
        let admin = connect().await;
        let tenant = new_tenant(&admin).await;
        let user = format!("short_{}", Uuid::new_v4().simple());
        let define = format!(
            "DEFINE USER {user} ON NAMESPACE PASSWORD 'short' ROLES EDITOR DURATION FOR TOKEN 1s"
        );
        let admin_session = admin.system_session(&tenant).await.unwrap();
        admin_session.query(define).await.unwrap().check().unwrap();
        let port = std::env::var("SURREAL_PORT").unwrap_or_else(|_| "18000".into());
        let db = Db::connect(Config {
            endpoint: format!("ws://127.0.0.1:{port}"),
            namespace: "resysto-dev".into(),
            username: user.clone(),
            password: "short".into(),
            signing_key: SIGNING_KEY.into(),
            verifying_key: VERIFYING_KEY.into(),
        })
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;
        // an idle server finds its system session stale on the next request
        let stale = System {
            session: db.0.bare.clone(),
            renew_at: Instant::now(),
        };
        *db.0.system.write().await = Arc::new(stale);
        let session = db.system_session(&tenant).await;

        admin_session
            .query(format!("REMOVE USER {user} ON NAMESPACE"))
            .await
            .unwrap();
        let session = session.unwrap();
        let tenant_seen: Option<String> = session
            .query("RETURN session::db()")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(tenant_seen, Some(tenant));
    }

    #[test]
    fn accepts_only_plain_identifiers() {
        assert!(check_name("t_acme_0a1b2c3d").is_ok());
        for name in [
            "",
            "T_acme",
            "t-acme",
            "t acme",
            "t`; REMOVE NAMESPACE x; --",
        ] {
            assert!(check_name(name).is_err(), "accepted {name:?}");
        }
    }
}
