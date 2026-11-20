use resysto_surrealdb::Db;
use secrecy::SecretString;
use serde::Deserialize;

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

// the docker stack, as the migrator: tests create their own databases
pub(crate) async fn server() -> Db {
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
