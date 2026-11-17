use resysto_model::Email;
use resysto_surrealdb::transaction;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Bytes, RecordId, SurrealValue};
use tokio::task;

use super::account::{AccountId, active, check_password};
use super::error::AuthError;
use super::membership::memberships;
use super::session::{Session, end_other_sessions};
use super::token::key;
use crate::password::Passwords;
use crate::totp::{self, Cipher, TotpSetup};

/// Who has to sign in to a tenant with a second factor
#[derive(Debug, Clone, Copy, PartialEq, Eq, SurrealValue)]
#[surreal(untagged, rename_all = "lowercase")]
pub enum MfaRequiredFor {
    Admins,
    Everyone,
}

/// Starts binding an authenticator app once `password` is right; replacing a bound one takes a
/// session that proved it
pub(crate) async fn set_up_totp(
    master: &Surreal<Any>,
    passwords: &Passwords,
    cipher: &Cipher,
    session: Session,
    password: String,
) -> Result<TotpSetup, AuthError> {
    let found = active(master, session.account).await?;
    if found.totp && !session.mfa {
        return Err(AuthError::SecondFactorNeeded);
    }
    check_password(master, passwords, &found, password).await?;
    let secret = totp::new_secret();
    let setup = totp::setup(&secret, found.email.as_str())?;
    let sealed = cipher.seal(session.account, &secret)?;
    master
        .query("UPDATE $account SET totp_pending = $sealed RETURN NONE")
        .bind(("account", session.account))
        .bind(("sealed", Bytes::from(sealed)))
        .await?
        .check()?;
    Ok(setup)
}

/// Binds the app `set_up_totp` started with once `code` comes from it, and returns new recovery
/// codes; the session counts as signed in with a second factor, and every other one ends
pub(crate) async fn confirm_totp(
    master: &Surreal<Any>,
    passwords: &Passwords,
    cipher: &Cipher,
    account: AccountId,
    token: &str,
    code: &str,
) -> Result<Vec<String>, AuthError> {
    let sealed: Option<Bytes> = master
        .query("SELECT VALUE totp_pending FROM ONLY $account")
        .bind(("account", account))
        .await?
        .check()?
        .take(0)?;
    let sealed = sealed.ok_or(AuthError::NoTotpSetup)?;
    // no failure counted: whoever asks chose this secret a moment ago
    let step = totp::step_of(&cipher.open(account, &sealed)?, code)?.ok_or(AuthError::WrongCode)?;
    let hasher = passwords.clone();
    let (codes, hashes) = task::spawn_blocking(move || {
        let codes = totp::recovery_codes();
        let hashes = codes
            .iter()
            .map(|code| hasher.hash_code(&totp::normalized_code(code)))
            .collect::<Result<Vec<_>, _>>();
        hashes.map(|hashes| (codes, hashes))
    })
    .await??;
    // only while the pending secret is this one: another setup may have replaced it meanwhile
    let enabled: Vec<RecordId> = master
        .query(
            "UPDATE $account
             SET totp = { secret: $sealed, last_step: $step, recovery_codes: $recovery_codes },
                 totp_pending = NONE
             WHERE totp_pending = $sealed
             RETURN VALUE id",
        )
        .bind(("account", account))
        .bind(("sealed", sealed))
        .bind(("step", step))
        .bind(("recovery_codes", hashes))
        .await?
        .check()?
        .take(0)?;
    if enabled.is_empty() {
        return Err(AuthError::NoTotpSetup);
    }
    master
        .query("UPDATE type::record('session', $key) SET mfa = true RETURN NONE")
        .bind(("key", key(token)))
        .await?
        .check()?;
    end_other_sessions(master, account, token).await?;
    tracing::info!(account = %account, "authenticator app bound");
    Ok(codes)
}

/// Unbinds the authenticator app once `password` is right, unless a tenant requires one
pub(crate) async fn disable_totp(
    master: &Surreal<Any>,
    passwords: &Passwords,
    session: Session,
    password: String,
) -> Result<(), AuthError> {
    if !session.mfa {
        return Err(AuthError::SecondFactorNeeded);
    }
    let reachable = memberships(master, session.account).await?;
    if reachable.iter().any(|member| member.mfa_required) {
        return Err(AuthError::TotpRequired);
    }
    let found = active(master, session.account).await?;
    check_password(master, passwords, &found, password).await?;
    master
        .query("UPDATE $account SET totp = NONE, totp_pending = NONE RETURN NONE")
        .bind(("account", session.account))
        .await?
        .check()?;
    tracing::warn!(account = %session.account, "authenticator app unbound");
    Ok(())
}

/// Unbinds the authenticator app of the account `email` names and ends its sessions, for an
/// account that lost it
pub async fn reset_totp(master: &Surreal<Any>, email: &Email) -> Result<(), AuthError> {
    let reset = transaction(master, |tx| {
        let email = email.clone();
        async move {
            let found: Option<AccountId> = tx
                .query(
                    "UPDATE account SET totp = NONE, totp_pending = NONE WHERE email = $email
                     RETURN VALUE id",
                )
                .bind(("email", email))
                .await?
                .check()?
                .take(0)?;
            let Some(account) = found else {
                return Ok(None);
            };
            // its sessions and its unfinished logins
            tx.query(
                "DELETE session WHERE account = $account;
                 DELETE login_challenge WHERE account = $account;",
            )
            .bind(("account", account))
            .await?
            .check()?;
            Ok(Some(account))
        }
    })
    .await?;
    let account = reset.ok_or_else(|| AuthError::UnknownAccount(email.to_string()))?;
    tracing::warn!(account = %account, "authenticator app reset");
    Ok(())
}

/// Sets who has to sign in to the tenant `slug` names with a second factor; sessions already open
/// without one stop reaching it
pub async fn require_mfa(
    master: &Surreal<Any>,
    slug: &str,
    required_for: MfaRequiredFor,
) -> Result<(), AuthError> {
    let updated: Vec<RecordId> = master
        .query(
            "UPDATE tenant SET mfa_required_for = $required_for WHERE slug = $slug
             RETURN VALUE id",
        )
        .bind(("slug", slug.to_owned()))
        .bind(("required_for", required_for))
        .await?
        .check()?
        .take(0)?;
    if updated.is_empty() {
        return Err(AuthError::UnknownTenant(slug.to_owned()));
    }
    tracing::info!(
        tenant = slug,
        ?required_for,
        "second factor requirement changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{create_account, profile};
    use crate::policy::ADMIN;
    use crate::session::authenticate;
    use crate::testing::{
        PASSWORD, ada, bob, cipher, code, enroll, master, passwords, session_token, sign_in,
    };

    #[tokio::test]
    async fn binding_an_app_takes_the_password_and_a_code_from_it() {
        let master = master().await;
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into(), None, &[])
            .await
            .unwrap();
        let token = session_token(sign_in(&master, ada()).await);
        let session = authenticate(&master, &token).await.unwrap().unwrap();
        let (passwords, cipher) = (passwords(), cipher());
        let set_up = async |password: &str| {
            set_up_totp(&master, &passwords, &cipher, session, password.into()).await
        };
        let confirm = async |code: String| {
            confirm_totp(&master, &passwords, &cipher, account, &token, &code).await
        };

        let early = confirm("123456".into()).await;
        assert!(
            matches!(early, Err(AuthError::NoTotpSetup)),
            "{:?}",
            early.err()
        );
        let refused = set_up("wrong horse battery staple").await;
        assert!(
            matches!(refused, Err(AuthError::WrongPassword)),
            "{:?}",
            refused.err()
        );
        let setup = set_up(PASSWORD).await.unwrap();
        let shown: u32 = code(&setup.secret, 0).parse().unwrap();
        let wrong = confirm(format!("{:06}", (shown + 1) % 1_000_000)).await;
        assert!(
            matches!(wrong, Err(AuthError::WrongCode)),
            "{:?}",
            wrong.err()
        );
        assert!(!profile(&master, account).await.unwrap().unwrap().totp);
        confirm(code(&setup.secret, 0)).await.unwrap();
        assert!(profile(&master, account).await.unwrap().unwrap().totp);

        // replacing the app takes a session that proved the old one
        let password_only = Session {
            account,
            mfa: false,
        };
        let replaced =
            set_up_totp(&master, &passwords, &cipher, password_only, PASSWORD.into()).await;
        assert!(
            matches!(replaced, Err(AuthError::SecondFactorNeeded)),
            "{:?}",
            replaced.err()
        );
    }

    #[tokio::test]
    async fn a_tenant_requires_a_second_factor_from_its_admins_or_from_everyone() {
        let master = master().await;
        master
            .query("CREATE tenant SET slug = 'one', db_name = 't_one', state = 'ready'")
            .await
            .unwrap()
            .check()
            .unwrap();
        let passwords = passwords();
        let roles = |role: &str| vec![role.to_owned()];
        let ada_id = create_account(
            &master,
            &passwords,
            ada(),
            PASSWORD.into(),
            Some("one"),
            &roles(ADMIN),
        )
        .await
        .unwrap();
        let bob_id = create_account(
            &master,
            &passwords,
            bob(),
            PASSWORD.into(),
            Some("one"),
            &roles("tenant/reader"),
        )
        .await
        .unwrap();
        let required = async |account| -> Vec<bool> {
            let reachable = memberships(&master, account).await.unwrap();
            reachable.iter().map(|member| member.mfa_required).collect()
        };
        assert_eq!(required(ada_id).await, [true]);
        assert_eq!(required(bob_id).await, [false]);

        require_mfa(&master, "one", MfaRequiredFor::Everyone)
            .await
            .unwrap();
        assert_eq!(required(bob_id).await, [true]);
        let unknown = require_mfa(&master, "nobody", MfaRequiredFor::Everyone).await;
        assert!(
            matches!(unknown, Err(AuthError::UnknownTenant(_))),
            "{:?}",
            unknown.err()
        );

        let token = session_token(sign_in(&master, bob()).await);
        enroll(&master, &token).await;
        let session = authenticate(&master, &token).await.unwrap().unwrap();
        let kept = disable_totp(&master, &passwords, session, PASSWORD.into()).await;
        assert!(
            matches!(kept, Err(AuthError::TotpRequired)),
            "{:?}",
            kept.err()
        );
        require_mfa(&master, "one", MfaRequiredFor::Admins)
            .await
            .unwrap();
        disable_totp(&master, &passwords, session, PASSWORD.into())
            .await
            .unwrap();
        assert!(!profile(&master, bob_id).await.unwrap().unwrap().totp);
    }
}
