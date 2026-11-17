use std::time::Duration;

use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Datetime, RecordId, SurrealValue};

use super::account::AccountId;
use super::error::AuthError;
use super::token::{Token, key};

pub(crate) const IDLE: Duration = Duration::from_secs(60 * 60);
const LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// How stale `last_seen_at` gets before a request refreshes it, so most requests only read
const TOUCH: Duration = Duration::from_secs(60);

/// How many factors a login proved
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Factors {
    One,
    Two,
}

/// A live session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) account: AccountId,
    /// Whether its login proved a second factor
    pub(crate) mfa: bool,
}

/// A live session of an account
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionRecord {
    /// The SHA-256 of the session's token, never the token
    pub(crate) id: String,
    pub(crate) created_at: Datetime,
    pub(crate) last_seen_at: Datetime,
    pub(crate) user_agent: Option<String>,
    pub(crate) mfa: bool,
    /// The session of the request that asked
    pub(crate) current: bool,
}

#[derive(SurrealValue)]
struct LiveSession {
    account: AccountId,
    stale: bool,
    mfa: bool,
}

#[derive(SurrealValue)]
struct SessionRow {
    key: String,
    created_at: Datetime,
    last_seen_at: Datetime,
    user_agent: Option<String>,
    mfa: bool,
}

pub(super) async fn open(
    master: &Surreal<Any>,
    account: AccountId,
    user_agent: Option<String>,
    factors: Factors,
) -> Result<Token, AuthError> {
    let token = Token::new();
    master
        .query(
            "CREATE type::record('session', $key)
             SET account = $account, user_agent = $user_agent, mfa = $mfa,
                 last_seen_at = time::now(), expires_at = time::now() + $lifetime
             RETURN NONE",
        )
        .bind(("key", token.key()))
        .bind(("account", account))
        .bind(("user_agent", user_agent))
        .bind(("mfa", factors == Factors::Two))
        .bind(("lifetime", LIFETIME))
        .await?
        .check()?;
    Ok(token)
}

pub(crate) async fn authenticate(
    master: &Surreal<Any>,
    token: &str,
) -> Result<Option<Session>, AuthError> {
    let key = key(token);
    let found: Option<LiveSession> = master
        .query(
            "SELECT account, mfa, last_seen_at < time::now() - $touch AS stale
             FROM type::record('session', $key)
             WHERE expires_at > time::now() AND last_seen_at > time::now() - $idle AND account.active",
        )
        .bind(("key", key.clone()))
        .bind(("touch", TOUCH))
        .bind(("idle", IDLE))
        .await?
        .check()?
        .take(0)?;
    let Some(session) = found else {
        return Ok(None);
    };
    if session.stale {
        let touched = master
            .query(
                "UPDATE type::record('session', $key) SET last_seen_at = time::now() RETURN NONE",
            )
            .bind(("key", key))
            .await
            .and_then(|response| response.check());
        // parallel requests of one session can conflict on the refresh; a later request retries it
        if let Err(error) = touched {
            tracing::warn!(%error, "session refresh failed");
        }
    }
    Ok(Some(Session {
        account: session.account,
        mfa: session.mfa,
    }))
}

pub(crate) async fn logout(master: &Surreal<Any>, token: &str) -> Result<(), AuthError> {
    master
        .query("DELETE type::record('session', $key)")
        .bind(("key", key(token)))
        .await?
        .check()?;
    tracing::info!("logout");
    Ok(())
}

/// The live sessions of the account, the most recently used first
pub(crate) async fn sessions(
    master: &Surreal<Any>,
    account: AccountId,
    token: &str,
) -> Result<Vec<SessionRecord>, AuthError> {
    let current = key(token);
    let rows: Vec<SessionRow> = master
        .query(
            "SELECT record::id(id) AS key, created_at, last_seen_at, user_agent, mfa FROM session
             WHERE account = $account AND expires_at > time::now()
                 AND last_seen_at > time::now() - $idle
             ORDER BY last_seen_at DESC",
        )
        .bind(("account", account))
        .bind(("idle", IDLE))
        .await?
        .check()?
        .take(0)?;
    Ok(rows
        .into_iter()
        .map(|row| SessionRecord {
            current: row.key == current,
            id: row.key,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            user_agent: row.user_agent,
            mfa: row.mfa,
        })
        .collect())
}

/// Ends one session of the account; false when the account has no such session
pub(crate) async fn end_session(
    master: &Surreal<Any>,
    account: AccountId,
    id: &str,
) -> Result<bool, AuthError> {
    let deleted: Vec<RecordId> = master
        .query(
            "DELETE type::record('session', $key) WHERE account = $account
             RETURN VALUE $before.id",
        )
        .bind(("key", id.to_owned()))
        .bind(("account", account))
        .await?
        .check()?
        .take(0)?;
    let ended = !deleted.is_empty();
    if ended {
        tracing::info!(account = %account, "session ended");
    }
    Ok(ended)
}

/// Ends every session of the account but the one `token` belongs to, and returns how many went
pub(crate) async fn end_other_sessions(
    master: &Surreal<Any>,
    account: AccountId,
    token: &str,
) -> Result<usize, AuthError> {
    let deleted: Vec<RecordId> = master
        .query(
            "DELETE session WHERE account = $account AND id != type::record('session', $keep)
             RETURN VALUE $before.id",
        )
        .bind(("account", account))
        .bind(("keep", key(token)))
        .await?
        .check()?
        .take(0)?;
    let ended = deleted.len();
    tracing::info!(account = %account, ended, "other sessions ended");
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use resysto_model::Email;

    use super::*;
    use crate::account::create_account;
    use crate::login::login;
    use crate::testing::{PASSWORD, ada, bob, master, passwords, run, session_token, sign_in};

    #[tokio::test]
    async fn a_session_ends_at_logout_after_an_idle_hour_and_after_a_day() {
        let master = master().await;
        create_account(&master, &passwords(), ada(), PASSWORD.into(), None, &[])
            .await
            .unwrap();
        let mut tokens = Vec::new();
        for _ in 0..4 {
            tokens.push(session_token(sign_in(&master, ada()).await));
        }
        let [logged_out, idle, expired, used] = <[String; 4]>::try_from(tokens).unwrap();

        logout(&master, &logged_out).await.unwrap();
        let idle_since = "UPDATE type::record('session', $key) SET last_seen_at = time::now() - 2h";
        run(&master, idle_since, &key(&idle)).await;
        let expired_at = "UPDATE type::record('session', $key) SET expires_at = time::now() - 1s";
        run(&master, expired_at, &key(&expired)).await;
        // stale but not idle: using it moves last_seen_at forward
        let seen_at = "UPDATE type::record('session', $key) SET last_seen_at = time::now() - 50m";
        run(&master, seen_at, &key(&used)).await;

        for token in [&logged_out, &idle, &expired] {
            assert_eq!(authenticate(&master, token).await.unwrap(), None);
        }
        assert!(authenticate(&master, &used).await.unwrap().is_some());
        let fresh: Option<bool> = master
            .query(
                "SELECT VALUE last_seen_at > time::now() - $touch
                 FROM type::record('session', $key)",
            )
            .bind(("touch", TOUCH))
            .bind(("key", key(&used)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(fresh, Some(true));
    }

    #[tokio::test]
    async fn an_account_sees_and_ends_only_its_own_sessions() {
        let master = master().await;
        let passwords = passwords();
        let account = create_account(&master, &passwords, ada(), PASSWORD.into(), None, &[])
            .await
            .unwrap();
        create_account(&master, &passwords, bob(), PASSWORD.into(), None, &[])
            .await
            .unwrap();
        let session = |email: Email, agent: &str| {
            let agent = Some(agent.to_owned());
            let passwords = &passwords;
            let master = &master;
            async move {
                session_token(
                    login(master, passwords, email, PASSWORD.into(), agent)
                        .await
                        .unwrap(),
                )
            }
        };
        let this = session(ada(), "Firefox").await;
        let other = session(ada(), "Safari").await;
        let bobs = session(bob(), "Chrome").await;

        let listed = sessions(&master, account, &this).await.unwrap();
        let mut seen: Vec<(&str, bool)> = listed
            .iter()
            .map(|session| {
                (
                    session.user_agent.as_deref().unwrap_or_default(),
                    session.current,
                )
            })
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, [("Firefox", true), ("Safari", false)]);

        assert!(!end_session(&master, account, &key(&bobs)).await.unwrap());
        assert!(authenticate(&master, &bobs).await.unwrap().is_some());
        assert!(end_session(&master, account, &key(&other)).await.unwrap());
        assert_eq!(authenticate(&master, &other).await.unwrap(), None);

        let again = session(ada(), "Edge").await;
        assert_eq!(
            end_other_sessions(&master, account, &this).await.unwrap(),
            1
        );
        assert_eq!(authenticate(&master, &again).await.unwrap(), None);
        assert!(authenticate(&master, &this).await.unwrap().is_some());
    }
}
