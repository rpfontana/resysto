use tokio::task::JoinError;
use totp_rs::TotpError;

use crate::password::PasswordError;
use crate::totp::CipherError;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid email or password")]
    InvalidCredentials,
    #[error("the login expired, sign in again")]
    LoginExpired,
    #[error("too many failed logins, try again later")]
    LoginLocked { retry_after: u64 },
    #[error("too many failed logins, an administrator has to unlock the account")]
    LoginBlocked,
    #[error("the current password is wrong")]
    WrongPassword,
    #[error("the code is wrong")]
    WrongCode,
    #[error("this needs a session signed in with a second factor")]
    SecondFactorNeeded,
    #[error("a tenant of the account requires a second factor")]
    TotpRequired,
    #[error("no authenticator app is being set up")]
    NoTotpSetup,
    #[error("no account has the email {0}")]
    UnknownAccount(String),
    #[error("no tenant has the slug {0}")]
    UnknownTenant(String),
    #[error("no role is called {0}")]
    UnknownRole(String),
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error(transparent)]
    Cipher(#[from] CipherError),
    #[error("TOTP failed: {0}")]
    Totp(#[from] TotpError),
    #[error("password hashing stopped: {0}")]
    Hashing(#[from] JoinError),
    #[error(transparent)]
    Db(#[from] resysto_surrealdb::Error),
}

impl From<surrealdb::Error> for AuthError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Db(error.into())
    }
}
