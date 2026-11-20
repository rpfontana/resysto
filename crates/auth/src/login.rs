use std::time::Duration;

use resysto_model::Email;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Bytes, RecordId, SurrealValue};
use tokio::task;

use super::account::{AccountId, AccountRecord, Credentials};
use super::audit::{self, About, Client, Event};
use super::error::AuthError;
use super::session::{self, Factors};
use super::throttle::{clear_failures, count_failure, ensure_not_locked};
use super::token::{Token, key};
use crate::password::Passwords;
use crate::totp::{self, Cipher};

/// How long a login whose password was right waits for its second factor
const CHALLENGE: Duration = Duration::from_secs(5 * 60);

/// Where a right password leads
pub(crate) enum Login {
    /// The account has no authenticator app, and a session is open
    SignedIn(Token, AccountRecord),
    /// The account has one, and a challenge waits for a code from it
    SecondFactor(Token),
}

#[derive(SurrealValue)]
struct Challenge {
    account: AccountId,
    email: Email,
    user_agent: Option<String>,
}

/// Checks the password of the active account `email` names; with an authenticator app bound, the
/// login then waits for a code from it
pub(crate) async fn login(
    master: &Surreal<Any>,
    passwords: &Passwords,
    email: Email,
    password: String,
    client: &Client,
) -> Result<Login, AuthError> {
    let attempts = key(email.as_str());
    ensure_not_locked(master, &attempts).await?;
    let found: Option<Credentials> = master
        .query(
            "SELECT id, email, password_hash, totp != NONE AS totp FROM account
             WHERE email = $email AND active LIMIT 1",
        )
        .bind(("email", email))
        .await?
        .check()?
        .take(0)?;
    let (account, totp, hash) = match found {
        Some(found) => (
            Some(AccountRecord {
                id: found.id,
                email: found.email,
            }),
            found.totp,
            Some(found.password_hash),
        ),
        None => (None, false, None),
    };
    let passwords = passwords.clone();
    let matches =
        task::spawn_blocking(move || passwords.verify(&password, hash.as_deref())).await?;
    let known = account.as_ref().map(|account| account.id);
    let Some(account) = account.filter(|_| matches) else {
        let failures = count_failure(master, &attempts).await?;
        let about = About {
            account: known,
            ..About::default()
        }
        .with_client(client);
        audit::record(master, Event::LoginFailed, about).await?;
        tracing::warn!(account = ?known, failures, "login failed");
        return Err(AuthError::InvalidCredentials);
    };
    if totp {
        let challenge = Token::new();
        master
            .query(
                "CREATE type::record('login_challenge', $key)
                 SET account = $account, user_agent = $user_agent, expires_at = time::now() + $wait
                 RETURN NONE",
            )
            .bind(("key", challenge.key()))
            .bind(("account", account.id))
            .bind(("user_agent", client.user_agent.clone()))
            .bind(("wait", CHALLENGE))
            .await?
            .check()?;
        tracing::info!(account = %account.id, "password accepted, waiting for a second factor");
        return Ok(Login::SecondFactor(challenge));
    }
    let user_agent = client.user_agent.clone();
    let token = open_session(master, &attempts, account.id, user_agent, Factors::One).await?;
    audit::record(
        master,
        Event::Login,
        About::of(account.id)
            .in_session(token.key())
            .with_client(client),
    )
    .await?;
    tracing::info!(account = %account.id, "login");
    Ok(Login::SignedIn(token, account))
}

/// Completes a login waiting on `challenge` with a code from the authenticator app
pub(crate) async fn login_with_totp(
    master: &Surreal<Any>,
    cipher: &Cipher,
    challenge: &str,
    code: &str,
    client: &Client,
) -> Result<(Token, AccountRecord), AuthError> {
    let (pending, attempts) = waiting(master, challenge).await?;
    let secret: Option<Bytes> = master
        .query("SELECT VALUE totp.secret FROM $account WHERE totp != NONE")
        .bind(("account", pending.account))
        .await?
        .check()?
        .take(0)?;
    let step = match secret {
        Some(secret) => totp::step_of(&cipher.open(pending.account, &secret)?, code)?,
        None => None,
    };
    // a code works once: its step has to come after the last one accepted
    let accepted = match step {
        Some(step) => {
            let advanced: Vec<RecordId> = master
                .query(
                    "UPDATE $account SET totp.last_step = $step WHERE totp.last_step < $step
                     RETURN VALUE id",
                )
                .bind(("account", pending.account))
                .bind(("step", step))
                .await?
                .check()?
                .take(0)?;
            !advanced.is_empty()
        }
        None => false,
    };
    if !accepted {
        return Err(refuse_code(master, &attempts, pending.account, client).await?);
    }
    complete_login(master, challenge, pending, &attempts, client).await
}

/// Completes a login waiting on `challenge` with a recovery code, which then stops working
pub(crate) async fn login_with_recovery_code(
    master: &Surreal<Any>,
    passwords: &Passwords,
    challenge: &str,
    code: &str,
    client: &Client,
) -> Result<(Token, AccountRecord), AuthError> {
    let (pending, attempts) = waiting(master, challenge).await?;
    let hashes: Option<Vec<String>> = master
        .query("SELECT VALUE totp.recovery_codes FROM $account WHERE totp != NONE")
        .bind(("account", pending.account))
        .await?
        .check()?
        .take(0)?;
    let hashes = hashes.unwrap_or_default();
    let code = totp::normalized_code(code);
    let passwords = passwords.clone();
    let matched = task::spawn_blocking(move || {
        hashes
            .into_iter()
            .find(|hash| passwords.verify(&code, Some(hash)))
    })
    .await?;
    // how many codes are left; none when another login used this one meanwhile
    let left: Option<i64> = match matched {
        Some(hash) => master
            .query(
                "UPDATE $account SET totp.recovery_codes -= $hash
                 WHERE totp.recovery_codes CONTAINS $hash
                 RETURN VALUE array::len(totp.recovery_codes)",
            )
            .bind(("account", pending.account))
            .bind(("hash", hash))
            .await?
            .check()?
            .take(0)?,
        None => None,
    };
    let Some(left) = left else {
        return Err(refuse_code(master, &attempts, pending.account, client).await?);
    };
    let about = About::of(pending.account).with_client(client);
    audit::record(master, Event::RecoveryCodeUsed, about).await?;
    tracing::warn!(account = %pending.account, left, "recovery code used");
    complete_login(master, challenge, pending, &attempts, client).await
}

// the challenge still open behind the cookie, and the key its email's failures are counted by
async fn waiting(master: &Surreal<Any>, challenge: &str) -> Result<(Challenge, String), AuthError> {
    let pending: Option<Challenge> = master
        .query(
            "SELECT account, account.email AS email, user_agent
             FROM type::record('login_challenge', $key)
             WHERE expires_at > time::now() AND account.active",
        )
        .bind(("key", key(challenge)))
        .await?
        .check()?
        .take(0)?;
    let pending = pending.ok_or(AuthError::LoginExpired)?;
    let attempts = key(pending.email.as_str());
    ensure_not_locked(master, &attempts).await?;
    Ok((pending, attempts))
}

async fn refuse_code(
    master: &Surreal<Any>,
    attempts: &str,
    account: AccountId,
    client: &Client,
) -> Result<AuthError, AuthError> {
    let failures = count_failure(master, attempts).await?;
    let about = About::of(account).with_client(client);
    audit::record(master, Event::SecondFactorRefused, about).await?;
    tracing::warn!(account = %account, failures, "second factor refused");
    Ok(AuthError::WrongCode)
}

async fn complete_login(
    master: &Surreal<Any>,
    challenge: &str,
    pending: Challenge,
    attempts: &str,
    client: &Client,
) -> Result<(Token, AccountRecord), AuthError> {
    master
        .query("DELETE type::record('login_challenge', $key)")
        .bind(("key", key(challenge)))
        .await?
        .check()?;
    let token = open_session(
        master,
        attempts,
        pending.account,
        pending.user_agent,
        Factors::Two,
    )
    .await?;
    audit::record(
        master,
        Event::Login,
        About::of(pending.account)
            .in_session(token.key())
            .with_client(client),
    )
    .await?;
    tracing::info!(account = %pending.account, "login with a second factor");
    let account = AccountRecord {
        id: pending.account,
        email: pending.email,
    };
    Ok((token, account))
}

// failures clear only once a login is complete, or a known password would reset the count
// between guessed codes
async fn open_session(
    master: &Surreal<Any>,
    attempts: &str,
    account: AccountId,
    user_agent: Option<String>,
    factors: Factors,
) -> Result<Token, AuthError> {
    clear_failures(master, attempts).await?;
    session::open(master, account, user_agent, factors).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{create_account, profile};
    use crate::audit::Client;
    use crate::mfa::reset_totp;
    use crate::session::{Session, authenticate};
    use crate::testing::{
        PASSWORD, ada, bob, challenge_token, cipher, code, enroll, master, passwords,
        session_token, sign_in,
    };

    #[tokio::test]
    async fn login_needs_the_password_of_an_active_account() {
        let master = master().await;
        create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();

        let attempts = [
            (ada(), "wrong horse battery staple"),
            ("bob@acme.it".parse().unwrap(), PASSWORD),
        ];
        for (email, password) in attempts {
            let refused = login(
                &master,
                &passwords(),
                email,
                password.into(),
                &Client::default(),
            )
            .await;
            assert!(
                matches!(refused, Err(AuthError::InvalidCredentials)),
                "{password}: {:?}",
                refused.err()
            );
        }
        assert!(
            login(
                &master,
                &passwords(),
                ada(),
                PASSWORD.into(),
                &Client::default()
            )
            .await
            .is_ok()
        );

        master
            .query("UPDATE account SET active = false")
            .await
            .unwrap()
            .check()
            .unwrap();
        let refused = login(
            &master,
            &passwords(),
            ada(),
            PASSWORD.into(),
            &Client::default(),
        )
        .await;
        assert!(
            matches!(refused, Err(AuthError::InvalidCredentials)),
            "{:?}",
            refused.err()
        );
    }

    #[tokio::test]
    async fn a_code_from_the_app_completes_a_login_once() {
        let master = master().await;
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();
        let first = session_token(sign_in(&master, ada()).await);
        let other = session_token(sign_in(&master, ada()).await);
        let (secret, _) = enroll(&master, &first).await;
        let two_factors = Some(Session { account, mfa: true });
        assert_eq!(authenticate(&master, &first).await.unwrap(), two_factors);
        assert_eq!(authenticate(&master, &other).await.unwrap(), None);

        let waiting = challenge_token(sign_in(&master, ada()).await);
        let shown: u32 = code(&secret, 0).parse().unwrap();
        let wrong = format!("{:06}", (shown + 1) % 1_000_000);
        // the code that bound the app was used already
        for refused in [wrong, code(&secret, 0)] {
            let answer =
                login_with_totp(&master, &cipher(), &waiting, &refused, &Client::default()).await;
            assert!(
                matches!(answer, Err(AuthError::WrongCode)),
                "{refused}: {:?}",
                answer.err()
            );
        }
        // a right password does not clear the failures while the code is missing
        let waiting = challenge_token(sign_in(&master, ada()).await);
        let failures = async || -> Option<i64> {
            master
                .query("SELECT VALUE failures FROM type::record('login_attempt', $key)")
                .bind(("key", key(ada().as_str())))
                .await
                .unwrap()
                .take(0)
                .unwrap()
        };
        assert_eq!(failures().await, Some(2));

        let (token, _) = login_with_totp(
            &master,
            &cipher(),
            &waiting,
            &code(&secret, 30),
            &Client::default(),
        )
        .await
        .unwrap();
        let token = token.into_string();
        assert_eq!(authenticate(&master, &token).await.unwrap(), two_factors);
        assert_eq!(failures().await, None);
        let spent = login_with_totp(
            &master,
            &cipher(),
            &waiting,
            &code(&secret, 30),
            &Client::default(),
        )
        .await;
        assert!(
            matches!(spent, Err(AuthError::LoginExpired)),
            "{:?}",
            spent.err()
        );
    }

    #[tokio::test]
    async fn a_recovery_code_completes_one_login_and_a_reset_unbinds_the_app() {
        let master = master().await;
        let passwords = passwords();
        let account = create_account(&master, &passwords, ada(), PASSWORD.into())
            .await
            .unwrap();
        let first = session_token(sign_in(&master, ada()).await);
        let (_, codes) = enroll(&master, &first).await;
        assert_eq!(codes.len(), 10);

        let waiting = challenge_token(sign_in(&master, ada()).await);
        let typed = codes[0].to_uppercase().replace('-', " ");
        let (token, _) =
            login_with_recovery_code(&master, &passwords, &waiting, &typed, &Client::default())
                .await
                .unwrap();
        let token = token.into_string();
        let session = authenticate(&master, &token).await.unwrap();
        assert_eq!(session, Some(Session { account, mfa: true }));
        let waiting = challenge_token(sign_in(&master, ada()).await);
        let used =
            login_with_recovery_code(&master, &passwords, &waiting, &codes[0], &Client::default())
                .await;
        assert!(
            matches!(used, Err(AuthError::WrongCode)),
            "{:?}",
            used.err()
        );
        assert!(
            login_with_recovery_code(&master, &passwords, &waiting, &codes[1], &Client::default())
                .await
                .is_ok()
        );
        let left = profile(&master, account).await.unwrap().unwrap();
        assert_eq!(left.recovery_codes_left, 8);

        reset_totp(&master, &ada()).await.unwrap();
        assert_eq!(authenticate(&master, &token).await.unwrap(), None);
        let after = profile(&master, account).await.unwrap().unwrap();
        assert!(!after.totp);
        session_token(sign_in(&master, ada()).await);
        let unknown = reset_totp(&master, &bob()).await;
        assert!(
            matches!(unknown, Err(AuthError::UnknownAccount(_))),
            "{:?}",
            unknown.err()
        );
    }
}
