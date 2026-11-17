use std::time::Duration;

use resysto_surrealdb::{Db, Error, write_in_batches};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use tokio::time;

use super::session::IDLE;
use super::throttle::{FAILURES_KEPT, MAX_FAILURES};

const EVERY: Duration = Duration::from_secs(60 * 60);
/// Records a cleanup deletes per transaction
const BATCH: u32 = 1_000;

/// Deletes ended sessions, expired challenges and forgotten failures every hour, for as long as
/// the server runs
pub async fn sweep_forever(db: Db, master: String) {
    let mut every = time::interval(EVERY);
    loop {
        every.tick().await;
        let swept = match db.system_session(&master).await {
            Ok(session) => sweep(&session).await,
            Err(error) => Err(error),
        };
        match swept {
            Ok(0) => {}
            Ok(deleted) => tracing::info!(deleted, "swept ended sessions and old login failures"),
            Err(error) => tracing::error!(%error, "sweeping sessions failed"),
        }
    }
}

/// Deletes the sessions that ended and the failures no login repeated for a while; an email
/// blocked by too many failures stays blocked
async fn sweep(master: &Surreal<Any>) -> Result<u64, Error> {
    let sessions = write_in_batches(BATCH, || {
        master
            .query(
                "LET $batch = (SELECT VALUE id FROM session
                     WHERE expires_at < time::now() OR last_seen_at < time::now() - $idle LIMIT $limit);
                 DELETE $batch;
                 RETURN $batch;",
            )
            .bind(("idle", IDLE))
    })
    .await?;
    let failures = write_in_batches(BATCH, || {
        master
            .query(
                "LET $batch = (SELECT VALUE id FROM login_attempt
                     WHERE updated_at < time::now() - $kept AND failures < $max LIMIT $limit);
                 DELETE $batch;
                 RETURN $batch;",
            )
            .bind(("kept", FAILURES_KEPT))
            .bind(("max", MAX_FAILURES))
    })
    .await?;
    let challenges = write_in_batches(BATCH, || {
        master.query(
            "LET $batch = (SELECT VALUE id FROM login_challenge
                 WHERE expires_at < time::now() LIMIT $limit);
             DELETE $batch;
             RETURN $batch;",
        )
    })
    .await?;
    Ok(sessions + failures + challenges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::create_account;
    use crate::session::authenticate;
    use crate::testing::{PASSWORD, ada, master, passwords, run, session_token, sign_in};
    use crate::token::key;

    #[tokio::test]
    async fn the_sweep_deletes_only_the_sessions_that_ended() {
        let master = master().await;
        let passwords = passwords();
        create_account(&master, &passwords, ada(), PASSWORD.into(), None, &[])
            .await
            .unwrap();
        let mut tokens = Vec::new();
        for _ in 0..3 {
            tokens.push(session_token(sign_in(&master, ada()).await));
        }
        let [live, idle, expired] = <[String; 3]>::try_from(tokens).unwrap();
        let idle_since = "UPDATE type::record('session', $key) SET last_seen_at = time::now() - 2h";
        run(&master, idle_since, &key(&idle)).await;
        let expired_at = "UPDATE type::record('session', $key) SET expires_at = time::now() - 1s";
        run(&master, expired_at, &key(&expired)).await;

        assert_eq!(sweep(&master).await.unwrap(), 2);
        assert!(authenticate(&master, &live).await.unwrap().is_some());
        let left: Option<i64> = master
            .query("RETURN count(SELECT id FROM session)")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(left, Some(1));
    }
}
