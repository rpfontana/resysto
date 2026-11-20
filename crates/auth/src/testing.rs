use std::time::{SystemTime, UNIX_EPOCH};

use resysto_model::Email;
use resysto_surrealdb::Db;
use secrecy::SecretString;
use serde::Deserialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::Uuid;
use totp_rs::{Builder, Secret};

use super::audit::Client;
use super::login::{Login, login};
use super::mfa::{confirm_totp, set_up_totp};
use super::session::authenticate;
use crate::password::Passwords;
use crate::totp::Cipher;

pub(super) const PASSWORD: &str = "correct horse battery staple";

#[derive(Deserialize)]
struct Config {
    db: DbConfig,
    auth: AuthConfig,
}

#[derive(Deserialize)]
struct DbConfig {
    endpoint: String,
    namespace: String,
    migrator_user: String,
    migrator_password: SecretString,
}

#[derive(Deserialize)]
struct AuthConfig {
    signing_key: SecretString,
    verifying_key: String,
}

pub(super) async fn server() -> Db {
    let Config { db, auth } = resysto_config::load().unwrap();
    Db::connect(resysto_surrealdb::Config {
        endpoint: db.endpoint,
        namespace: db.namespace,
        username: db.migrator_user,
        password: db.migrator_password,
        signing_key: auth.signing_key,
        verifying_key: auth.verifying_key,
    })
    .await
    .unwrap()
}

// a master of its own: its name, and a session on it
pub(super) async fn new_master(server: &Db) -> (String, Surreal<Any>) {
    let name = format!("t_test_{}", Uuid::new_v4().simple());
    resysto_migrator::provision_master(server, &name)
        .await
        .unwrap();
    let session = server.system_session(&name).await.unwrap();
    (name, session)
}

pub(super) async fn master() -> Surreal<Any> {
    new_master(&server().await).await.1
}

// a ready tenant registered in the master, with its database provisioned
pub(super) async fn new_tenant(server: &Db, master: &Surreal<Any>, slug: &str) -> String {
    let name = format!("t_test_{}", Uuid::new_v4().simple());
    resysto_migrator::provision_tenant(server, &name)
        .await
        .unwrap();
    master
        .query("CREATE tenant SET slug = $slug, db_name = $name, state = 'ready'")
        .bind(("slug", slug.to_owned()))
        .bind(("name", name.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();
    name
}

pub(super) fn passwords() -> Passwords {
    Passwords::new(SecretString::from("pepper")).unwrap()
}

pub(super) fn cipher() -> Cipher {
    Cipher::new(&SecretString::from("7".repeat(64))).unwrap()
}

pub(super) fn ada() -> Email {
    "ada@acme.it".parse().unwrap()
}

pub(super) fn bob() -> Email {
    "bob@acme.it".parse().unwrap()
}

pub(super) fn session_token(login: Login) -> String {
    match login {
        Login::SignedIn(token, _) => token.into_string(),
        Login::SecondFactor(_) => panic!("the login waits for a second factor"),
    }
}

pub(super) fn challenge_token(login: Login) -> String {
    match login {
        Login::SecondFactor(challenge) => challenge.into_string(),
        Login::SignedIn(..) => panic!("the login needs no second factor"),
    }
}

pub(super) async fn sign_in(master: &Surreal<Any>, email: Email) -> Login {
    login(
        master,
        &passwords(),
        email,
        PASSWORD.into(),
        &Client::default(),
    )
    .await
    .unwrap()
}

// what the authenticator app shows `later` seconds from now
pub(super) fn code(secret: &str, later: u64) -> String {
    let secret = Secret::try_from_base32(secret).unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let totp = Builder::new().with_secret(secret).build().unwrap();
    totp.generate(now.as_secs() + later).to_string()
}

// binds an app to the account the session belongs to: its secret and recovery codes
pub(super) async fn enroll(master: &Surreal<Any>, token: &str) -> (String, Vec<String>) {
    let session = authenticate(master, token).await.unwrap().unwrap();
    let setup = set_up_totp(
        master,
        &passwords(),
        &cipher(),
        session,
        PASSWORD.into(),
        &Client::default(),
    )
    .await
    .unwrap();
    let first = code(&setup.secret, 0);
    let codes = confirm_totp(
        master,
        &passwords(),
        &cipher(),
        session.account,
        token,
        &first,
        &Client::default(),
    )
    .await
    .unwrap();
    (setup.secret, codes)
}

pub(super) async fn run(master: &Surreal<Any>, query: &str, key: &str) {
    master
        .query(query)
        .bind(("key", key.to_owned()))
        .await
        .unwrap()
        .check()
        .unwrap();
}
