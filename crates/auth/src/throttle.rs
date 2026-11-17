use std::time::Duration;

use resysto_model::Email;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::SurrealValue;

use super::error::AuthError;
use super::token::key;

/// Failed logins in a row before each further attempt has to wait
const FREE_FAILURES: i64 = 5;
pub(crate) const MAX_FAILURES: i64 = 100;
/// The first wait, doubled at each further failure up to `MAX_WAIT`
const FIRST_WAIT: Duration = Duration::from_secs(60);
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);
/// Failures that no new one followed for this long are forgotten
pub(crate) const FAILURES_KEPT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(SurrealValue)]
struct LoginLock {
    failures: i64,
    /// Seconds left of the wait, while there is one
    wait: Option<i64>,
}

/// Forgets the failed logins of `email`, blocked or not
pub async fn unlock(master: &Surreal<Any>, email: &Email) -> Result<(), AuthError> {
    clear_failures(master, &key(email.as_str())).await?;
    tracing::info!("login unlocked");
    Ok(())
}

pub(super) async fn ensure_not_locked(
    master: &Surreal<Any>,
    attempts: &str,
) -> Result<(), AuthError> {
    let lock: Option<LoginLock> = master
        .query(
            "SELECT failures,
                 IF locked_until > time::now() THEN duration::secs(locked_until - time::now()) END AS wait
             FROM type::record('login_attempt', $key)",
        )
        .bind(("key", attempts.to_owned()))
        .await?
        .check()?
        .take(0)?;
    let Some(lock) = lock else {
        return Ok(());
    };
    if lock.failures >= MAX_FAILURES {
        tracing::warn!("password refused: the email is blocked");
        return Err(AuthError::LoginBlocked);
    }
    if let Some(wait) = lock.wait {
        tracing::warn!(wait, "password refused: the email has to wait");
        return Err(AuthError::LoginLocked {
            retry_after: u64::try_from(wait).unwrap_or_default().max(1),
        });
    }
    Ok(())
}

/// Counts a failed login and returns how many happened in a row
pub(super) async fn count_failure(master: &Surreal<Any>, attempts: &str) -> Result<i64, AuthError> {
    let failures: Option<i64> = master
        .query("UPSERT type::record('login_attempt', $key) SET failures += 1 RETURN VALUE failures")
        .bind(("key", attempts.to_owned()))
        .await?
        .check()?
        .take(0)?;
    let failures = failures.unwrap_or_default();
    if failures >= FREE_FAILURES {
        master
            .query(
                "UPDATE type::record('login_attempt', $key) SET locked_until = time::now() + $wait
                 RETURN NONE",
            )
            .bind(("key", attempts.to_owned()))
            .bind(("wait", wait_after(failures)))
            .await?
            .check()?;
    }
    Ok(failures)
}

pub(super) async fn clear_failures(master: &Surreal<Any>, attempts: &str) -> Result<(), AuthError> {
    master
        .query("DELETE type::record('login_attempt', $key)")
        .bind(("key", attempts.to_owned()))
        .await?
        .check()?;
    Ok(())
}

// each failure after the free ones doubles the wait, up to the longest
fn wait_after(failures: i64) -> Duration {
    let doublings = u32::try_from(failures - FREE_FAILURES).unwrap_or(u32::MAX);
    FIRST_WAIT
        .saturating_mul(2_u32.saturating_pow(doublings))
        .min(MAX_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::create_account;
    use crate::login::login;
    use crate::testing::{PASSWORD, ada, master, passwords, run};

    #[tokio::test]
    async fn failed_logins_make_an_email_wait_and_a_hundred_block_it_until_unlocked() {
        let master = master().await;
        let passwords = passwords();
        create_account(&master, &passwords, ada(), PASSWORD.into(), None, &[])
            .await
            .unwrap();
        let nobody: Email = "nobody@acme.it".parse().unwrap();
        let wrong = "wrong horse battery staple";

        // an email no account has gets the same answers, so a wait tells nothing about accounts
        for email in [ada(), nobody] {
            for attempt in 1..=5 {
                let refused = login(&master, &passwords, email.clone(), wrong.into(), None).await;
                assert!(
                    matches!(refused, Err(AuthError::InvalidCredentials)),
                    "{email} attempt {attempt}: {:?}",
                    refused.err()
                );
            }
            let waiting = login(&master, &passwords, email.clone(), PASSWORD.into(), None).await;
            assert!(
                matches!(
                    waiting,
                    Err(AuthError::LoginLocked {
                        retry_after: 1..=60
                    })
                ),
                "{email}: {:?}",
                waiting.err()
            );
        }

        let failures =
            "UPDATE type::record('login_attempt', $key) SET failures = 99, locked_until = NONE";
        run(&master, failures, &key(ada().as_str())).await;
        let refused = login(&master, &passwords, ada(), wrong.into(), None).await;
        assert!(
            matches!(refused, Err(AuthError::InvalidCredentials)),
            "{:?}",
            refused.err()
        );
        let blocked = login(&master, &passwords, ada(), PASSWORD.into(), None).await;
        assert!(
            matches!(blocked, Err(AuthError::LoginBlocked)),
            "{:?}",
            blocked.err()
        );

        unlock(&master, &ada()).await.unwrap();
        assert!(
            login(&master, &passwords, ada(), PASSWORD.into(), None)
                .await
                .is_ok()
        );
    }
}
