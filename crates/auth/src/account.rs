use resysto_id::{Entity, Id};
use resysto_model::Email;
use resysto_surrealdb::transaction;
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};
use tokio::task;

use super::error::AuthError;
use super::session::end_other_sessions;
use super::throttle::{clear_failures, count_failure, ensure_not_locked};
use super::token::key;
use crate::password::Passwords;
use crate::policy::ROLES;

#[derive(Debug)]
pub enum Account {}

impl Entity for Account {
    const TABLE: &'static str = "account";
}

pub type AccountId = Id<Account>;

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub(crate) struct AccountRecord {
    pub(crate) id: AccountId,
    pub(crate) email: Email,
}

/// An account as it sees itself
#[derive(Debug, Serialize, SurrealValue)]
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

/// Creates an account, a member with `roles` of the tenant `tenant` names when there is one
pub async fn create_account(
    master: &Surreal<Any>,
    passwords: &Passwords,
    email: Email,
    password: String,
    tenant: Option<&str>,
    roles: &[String],
) -> Result<AccountId, AuthError> {
    if let Some(unknown) = roles.iter().find(|role| !ROLES.contains(&role.as_str())) {
        return Err(AuthError::UnknownRole(unknown.clone()));
    }
    let tenant = match tenant {
        None => None,
        Some(slug) => {
            let found: Option<RecordId> = master
                .query("SELECT VALUE id FROM tenant WHERE slug = $slug LIMIT 1")
                .bind(("slug", slug.to_owned()))
                .await?
                .check()?
                .take(0)?;
            Some(found.ok_or_else(|| AuthError::UnknownTenant(slug.to_owned()))?)
        }
    };
    let passwords = passwords.clone();
    let hash = task::spawn_blocking(move || passwords.hash(&password)).await??;
    let id = AccountId::new();
    transaction(master, |tx| {
        let (email, hash, tenant, roles) =
            (email.clone(), hash.clone(), tenant.clone(), roles.to_vec());
        async move {
            tx.query("CREATE $id SET email = $email, password_hash = $password_hash RETURN NONE")
                .bind(("id", id))
                .bind(("email", email))
                .bind(("password_hash", hash))
                .await?
                .check()?;
            if let Some(tenant) = tenant {
                tx.query("RELATE $account->member_of->$tenant SET roles = $roles RETURN NONE")
                    .bind(("account", id))
                    .bind(("tenant", tenant))
                    .bind(("roles", roles))
                    .await?
                    .check()?;
            }
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
) -> Result<(), AuthError> {
    let found = active(master, account).await?;
    check_password(master, passwords, &found, current).await?;
    let hasher = passwords.clone();
    let hash = task::spawn_blocking(move || hasher.hash(&new)).await??;
    master
        .query("UPDATE $id SET password_hash = $password_hash RETURN NONE")
        .bind(("id", account))
        .bind(("password_hash", hash))
        .await?
        .check()?;
    end_other_sessions(master, account, token).await?;
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
) -> Result<(), AuthError> {
    let attempts = key(found.email.as_str());
    ensure_not_locked(master, &attempts).await?;
    let checker = passwords.clone();
    let hash = found.password_hash.clone();
    let matches = task::spawn_blocking(move || checker.verify(&password, Some(&hash))).await?;
    if !matches {
        let failures = count_failure(master, &attempts).await?;
        tracing::warn!(account = %found.id, failures, "password refused");
        return Err(AuthError::WrongPassword);
    }
    clear_failures(master, &attempts).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login::login;
    use crate::session::authenticate;
    use crate::testing::{PASSWORD, ada, master, passwords, session_token, sign_in};

    #[tokio::test]
    async fn changing_the_password_keeps_this_session_and_ends_the_others() {
        let master = master().await;
        let passwords = passwords();
        let account = create_account(&master, &passwords, ada(), PASSWORD.into(), None, &[])
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
        )
        .await
        .unwrap();

        assert!(authenticate(&master, &this).await.unwrap().is_some());
        assert_eq!(authenticate(&master, &other).await.unwrap(), None);
        let old = login(&master, &passwords, ada(), PASSWORD.into(), None).await;
        assert!(
            matches!(old, Err(AuthError::InvalidCredentials)),
            "{:?}",
            old.err()
        );
        assert!(
            login(&master, &passwords, ada(), new.into(), None)
                .await
                .is_ok()
        );
    }
}
