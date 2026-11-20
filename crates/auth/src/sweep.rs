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
/// How long the audit keeps a row, in the master and in every tenant
const AUDIT_KEPT: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// The audit tables of a tenant's database
const TENANT_AUDIT: [&str; 2] = ["audit_change", "audit_event"];

/// Deletes ended sessions, expired challenges, forgotten failures and audit rows older than a
/// year every hour, for as long as the server runs
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
        match sweep_audit(&db, &master).await {
            Ok(0) => {}
            Ok(deleted) => tracing::info!(deleted, "swept audit rows older than a year"),
            Err(error) => tracing::error!(%error, "sweeping the audit failed"),
        }
    }
}

// a tenant that fails is logged and skipped, so one broken database keeps no other one growing
async fn sweep_audit(db: &Db, master: &str) -> Result<u64, Error> {
    let session = db.system_session(master).await?;
    let mut deleted = forget_old(&session, "audit_event").await?;
    let tenants: Vec<String> = session
        .query("SELECT VALUE db_name FROM tenant WHERE state IN ['ready', 'suspended']")
        .await?
        .check()?
        .take(0)?;
    for database in tenants {
        let swept = async {
            let tenant = db.system_session(&database).await?;
            let mut deleted = 0;
            for table in TENANT_AUDIT {
                deleted += forget_old(&tenant, table).await?;
            }
            Ok::<_, Error>(deleted)
        }
        .await;
        match swept {
            Ok(count) => deleted += count,
            Err(error) => tracing::error!(%error, database, "sweeping a tenant's audit failed"),
        }
    }
    Ok(deleted)
}

// the keys are UUID v7, so the rows older than the cutoff are a range of keys and nothing newer
// is read
async fn forget_old(db: &Surreal<Any>, table: &'static str) -> Result<u64, Error> {
    write_in_batches(BATCH, || {
        db.query(
            "LET $batch = (SELECT VALUE id FROM type::record($table,
                 ..rand::uuid::v7(time::now() - $kept)) LIMIT $limit);
             DELETE $batch;
             RETURN $batch;",
        )
        .bind(("table", table))
        .bind(("kept", AUDIT_KEPT))
    })
    .await
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
    use crate::testing::{
        PASSWORD, ada, master, new_master, new_tenant, passwords, run, server, session_token,
        sign_in,
    };
    use crate::token::key;

    async fn events_left(db: &Surreal<Any>, table: &str) -> Vec<String> {
        db.query("SELECT VALUE event FROM type::table($table) ORDER BY at")
            .bind(("table", table.to_owned()))
            .await
            .unwrap()
            .take(0)
            .unwrap()
    }

    #[tokio::test]
    async fn the_audit_forgets_the_rows_older_than_a_year() {
        let server = server().await;
        let (name, master) = new_master(&server).await;
        let one = new_tenant(&server, &master, "one").await;
        let tenant = server.system_session(&one).await.unwrap();
        // the key keeps the time of the row; `at` only says it for the reader
        let aged = "CREATE type::record($table, rand::uuid::v7(time::now() - $age))
                    SET at = time::now() - $age, event = $event";
        let change = ", record = person:1";
        let rows = [
            (&master, "audit_event", "400d", "login", ""),
            (&master, "audit_event", "300d", "logout", ""),
            (&tenant, "audit_event", "366d", "refused", ""),
            (&tenant, "audit_change", "400d", "CREATE", change),
            (&tenant, "audit_change", "1d", "UPDATE", change),
        ];
        for (db, table, age, event, fields) in rows {
            db.query(format!("{aged}{fields}"))
                .bind(("table", table))
                .bind(("age", age.parse::<surrealdb::types::Duration>().unwrap()))
                .bind(("event", event))
                .await
                .unwrap()
                .check()
                .unwrap();
        }

        assert_eq!(sweep_audit(&server, &name).await.unwrap(), 3);

        assert_eq!(events_left(&master, "audit_event").await, ["logout"]);
        assert!(events_left(&tenant, "audit_event").await.is_empty());
        assert_eq!(events_left(&tenant, "audit_change").await, ["UPDATE"]);
    }

    #[tokio::test]
    async fn the_sweep_deletes_only_the_sessions_that_ended() {
        let master = master().await;
        let passwords = passwords();
        create_account(&master, &passwords, ada(), PASSWORD.into())
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
