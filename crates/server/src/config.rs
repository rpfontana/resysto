use axum_client_ip::ClientIpSource;
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct Config {
    pub(crate) addr: String,
    pub(crate) site: String,
    /// The URL clients reach the server at, which OAuth and MCP clients must see exactly
    pub(crate) public_url: String,
    /// Where the address of a request comes from: `ConnectInfo` without a proxy in front,
    /// the header the proxy sets behind one, such as `RightmostXForwardedFor`
    pub(crate) client_ip_source: ClientIpSource,
    pub(crate) db: Db,
    pub(crate) auth: Auth,
}

#[derive(Deserialize)]
pub(crate) struct Db {
    pub(crate) endpoint: String,
    pub(crate) namespace: String,
    pub(crate) app_user: String,
    pub(crate) app_password: SecretString,
}

#[derive(Deserialize)]
pub(crate) struct Auth {
    pub(crate) signing_key: SecretString,
    pub(crate) verifying_key: String,
    pub(crate) password_pepper: SecretString,
    pub(crate) encryption_key: SecretString,
}
