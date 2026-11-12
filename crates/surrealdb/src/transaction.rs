use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::method::{Query, Transaction};
use surrealdb::types::QueryError;

use crate::Error;

/// Waits before each rerun after a write conflict, so five attempts in all.
const BACKOFF: [Duration; 4] = [
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(160),
];

/// The open transaction handed to the closure of [`transaction`].
pub struct Tx(Arc<Transaction<Any>>);

impl Tx {
    /// Runs `sql` inside the transaction.
    pub fn query<'tx>(&'tx self, sql: impl Into<Cow<'tx, str>>) -> Query<'tx, Any> {
        self.0.query(sql)
    }
}

/// Runs `f` in a transaction on a clone of `db`, committing when it returns `Ok` and
/// cancelling when it returns `Err`.
///
/// A write conflict, from a statement or from the commit, cancels and reruns `f` from the start
/// after a short wait, up to five attempts in all; any other error is returned at once. `f`
/// therefore runs more than once and must have no effect outside the database.
///
/// # Errors
///
/// The error `f` returns, the last conflict when every attempt conflicts, a database error
/// while opening or committing, or [`Error::TxInUse`] when `f` keeps its [`Tx`] past the end
/// of its future.
pub async fn transaction<T, F, Fut>(db: &Surreal<Any>, mut f: F) -> Result<T, Error>
where
    F: FnMut(Tx) -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let mut waits = BACKOFF.into_iter();
    loop {
        match attempt(db, &mut f).await {
            Err(error) if is_conflict(&error) => match waits.next() {
                Some(wait) => tokio::time::sleep(wait).await,
                None => return Err(error),
            },
            outcome => return outcome,
        }
    }
}

async fn attempt<T, F, Fut>(db: &Surreal<Any>, f: &mut F) -> Result<T, Error>
where
    F: FnMut(Tx) -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let tx = Arc::new(db.clone().begin().await?);
    let outcome = f(Tx(Arc::clone(&tx))).await;
    let tx = Arc::into_inner(tx).ok_or(Error::TxInUse)?;
    match outcome {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            // A cancel that fails still rolls back when its future is dropped.
            let _ = tx.cancel().await;
            Err(error)
        }
    }
}

fn is_conflict(error: &Error) -> bool {
    matches!(error, Error::Db(e) if matches!(e.query_details(), Some(QueryError::TransactionConflict)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use surrealdb::types::Value;

    use super::*;
    use crate::tests::{connect, new_tenant};

    #[tokio::test]
    async fn a_write_conflict_reruns_the_closure_and_loses_no_update() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let session = db.system_session(&tenant).await.unwrap();
        let other = db.system_session(&tenant).await.unwrap();
        session
            .query("CREATE counter:c SET n = 0")
            .await
            .unwrap()
            .check()
            .unwrap();

        // Spawned so that the test stops compiling if the future is not Send.
        let task = tokio::spawn(async move {
            let attempts = AtomicUsize::new(0);
            let outcome = transaction(&session, |tx| {
                let first = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                let other = &other;
                async move {
                    let n: Option<i64> = tx
                        .query("SELECT VALUE n FROM ONLY counter:c")
                        .await?
                        .check()?
                        .take(0)?;
                    if first {
                        other.query("UPDATE counter:c SET n += 1").await?.check()?;
                    }
                    tx.query("UPDATE counter:c SET n = $n")
                        .bind(("n", n.unwrap_or_default() + 1))
                        .await?
                        .check()?;
                    Ok(())
                }
            })
            .await;
            (outcome, attempts.into_inner(), session)
        });
        let (outcome, attempts, session) = task.await.unwrap();

        outcome.unwrap();
        assert_eq!(attempts, 2);
        let n: Option<i64> = session
            .query("SELECT VALUE n FROM ONLY counter:c")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(n, Some(2), "an increment was lost");
    }

    #[tokio::test]
    async fn an_error_from_the_closure_keeps_none_of_its_writes_and_is_not_retried() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let session = db.system_session(&tenant).await.unwrap();
        session
            .query("DEFINE TABLE note")
            .await
            .unwrap()
            .check()
            .unwrap();

        let mut attempts = 0;
        let outcome: Result<(), Error> = transaction(&session, |tx| {
            attempts += 1;
            async move {
                tx.query("CREATE note:1").await?.check()?;
                Err(Error::InvalidName("abort".into()))
            }
        })
        .await;

        assert!(matches!(outcome, Err(Error::InvalidName(_))), "{outcome:?}");
        assert_eq!(attempts, 1);
        let notes: Vec<Value> = session
            .query("SELECT * FROM note")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(notes.is_empty(), "kept {notes:?}");
    }
}
