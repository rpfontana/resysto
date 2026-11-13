use secrecy::SecretString;
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct Settings {
    pub(crate) db: Db,
    pub(crate) auth: Auth,
}

#[derive(Deserialize)]
pub(crate) struct Db {
    pub(crate) endpoint: String,
    pub(crate) namespace: String,
    pub(crate) migrator_user: String,
    pub(crate) migrator_password: SecretString,
}

#[derive(Deserialize)]
pub(crate) struct Auth {
    pub(crate) signing_key: SecretString,
    pub(crate) verifying_key: String,
}
