use resysto_id::{Entity, Id};
use resysto_model::Email;
use resysto_surrealdb::transaction;
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::SurrealValue;
use tokio::task;
use utoipa::ToSchema;

use super::audit::{self, About, Client, Event};
use super::error::AuthError;
use super::session::end_other_sessions;
use super::throttle::{clear_failures, count_failure, ensure_not_locked};
use super::token::key;
use crate::password::Passwords;

#[derive(Debug)]
pub enum Account {}

impl Entity for Account {
    const TABLE: &'static str = "account";
}

pub type AccountId = Id<Account>;

#[derive(Debug, Clone, Serialize, SurrealValue, ToSchema)]
pub(crate) struct AccountRecord {
    pub(crate) id: AccountId,
    pub(crate) email: Email,
}

/// An account as it sees itself
#[derive(Debug, Serialize, SurrealValue, ToSchema)]
pub(crate) struct Profile {
    pub(crate) id: AccountId,
    pub(crate) email: Email,
    /// Whether an authenticator app is bound
    pub(crate) totp: bool,
    pub(crate) recovery_codes_left: i64,
}

#[derive(SurrealValue)]
pub(super) struct Credentials {
    pub(super) id: AccountId,
    pub(super) email: Email,
    pub(super) password_hash: String,
    /// Whether an authenticator app is bound
    pub(super) totp: bool,
}

/// Creates an account, a member of no tenant yet
pub async fn create_account(
    master: &Surreal<Any>,
    passwords: &Passwords,
    email: Email,
    password: String,
) -> Result<AccountId, AuthError> {
    let passwords = passwords.clone();
    let hash = task::spawn_blocking(move || passwords.hash(&password)).await??;
    let id = AccountId::new();
    transaction(master, |tx| {
        let (email, hash) = (email.clone(), hash.clone());
        async move {
            tx.query("CREATE $id SET email = $email, password_hash = $password_hash RETURN NONE")
                .bind(("id", id))
                .bind(("email", email))
                .bind(("password_hash", hash))
                .await?
                .check()?;
            audit::record(&tx, Event::AccountCreated, About::of(id)).await?;
            Ok(())
        }
    })
    .await?;
    Ok(id)
}

pub(crate) async fn profile(
    master: &Surreal<Any>,
    id: AccountId,
) -> Result<Option<Profile>, AuthError> {
    let found: Option<Profile> = master
        .query(
            "SELECT id, email, totp != NONE AS totp,
                 array::len(totp.recovery_codes ?? []) AS recovery_codes_left
             FROM ONLY $id",
        )
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(found)
}

/// Replaces the password when `current` is right, and ends every session of the account but the
/// one `token` belongs to
pub(crate) async fn change_password(
    master: &Surreal<Any>,
    passwords: &Passwords,
    account: AccountId,
    token: &str,
    current: String,
    new: String,
    client: &Client,
) -> Result<(), AuthError> {
    let found = active(master, account).await?;
    check_password(master, passwords, &found, current, client).await?;
    let hasher = passwords.clone();
    let hash = task::spawn_blocking(move || hasher.hash(&new)).await??;
    master
        .query("UPDATE $id SET password_hash = $password_hash RETURN NONE")
        .bind(("id", account))
        .bind(("password_hash", hash))
        .await?
        .check()?;
    end_other_sessions(master, account, token, client).await?;
    let about = About::of(account)
        .in_session(key(token))
        .with_client(client);
    audit::record(master, Event::PasswordChanged, about).await?;
    tracing::info!(account = %account, "password changed");
    Ok(())
}

pub(super) async fn active(
    master: &Surreal<Any>,
    account: AccountId,
) -> Result<Credentials, AuthError> {
    let found: Option<Credentials> = master
        .query("SELECT id, email, password_hash, totp != NONE AS totp FROM $id WHERE active")
        .bind(("id", account))
        .await?
        .check()?
        .take(0)?;
    found.ok_or(AuthError::InvalidCredentials)
}

// asked again before a change to the account; a wrong one counts as a failed login
pub(super) async fn check_password(
    master: &Surreal<Any>,
    passwords: &Passwords,
    found: &Credentials,
    password: String,
    client: &Client,
) -> Result<(), AuthError> {
    let attempts = key(found.email.as_str());
    ensure_not_locked(master, &attempts).await?;
    let checker = passwords.clone();
    let hash = found.password_hash.clone();
    let matches = task::spawn_blocking(move || checker.verify(&password, Some(&hash))).await?;
    if !matches {
        let failures = count_failure(master, &attempts).await?;
        let about = About::of(found.id).with_client(client);
        audit::record(master, Event::PasswordRefused, about).await?;
        tracing::warn!(account = %found.id, failures, "password refused");
        return Err(AuthError::WrongPassword);
    }
    clear_failures(master, &attempts).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::Client;
    use crate::login::login;
    use crate::session::authenticate;
    use crate::testing::{PASSWORD, ada, master, passwords, session_token, sign_in};

    #[tokio::test]
    async fn changing_the_password_keeps_this_session_and_ends_the_others() {
        let master = master().await;
        let passwords = passwords();
        let account = create_account(&master, &passwords, ada(), PASSWORD.into())
            .await
            .unwrap();
        let this = session_token(sign_in(&master, ada()).await);
        let other = session_token(sign_in(&master, ada()).await);
        let new = "a brand new passphrase";

        let refused = change_password(
            &master,
            &passwords,
            account,
            &this,
            "wrong".into(),
            new.into(),
            &Client::default(),
        )
        .await;
        assert!(
            matches!(refused, Err(AuthError::WrongPassword)),
            "{:?}",
            refused.err()
        );
        change_password(
            &master,
            &passwords,
            account,
            &this,
            PASSWORD.into(),
            new.into(),
            &Client::default(),
        )
        .await
        .unwrap();

        assert!(authenticate(&master, &this).await.unwrap().is_some());
        assert_eq!(authenticate(&master, &other).await.unwrap(), None);
        let old = login(
            &master,
            &passwords,
            ada(),
            PASSWORD.into(),
            &Client::default(),
        )
        .await;
        assert!(
            matches!(old, Err(AuthError::InvalidCredentials)),
            "{:?}",
            old.err()
        );
        assert!(
            login(&master, &passwords, ada(), new.into(), &Client::default())
                .await
                .is_ok()
        );
    }
}
