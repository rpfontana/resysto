use sha2::{Digest, Sha256};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{QueryError, SurrealValue};

use crate::Error;

/// A `.surql` file of a lane, embedded in the binary
#[derive(Debug, Clone, Copy)]
pub struct SchemaFile {
    /// Path inside the lane's `schema/` folder, and the key of the file's row in `__schema_file`
    pub path: &'static str,
    pub sql: &'static str,
}

const SCHEMA_FILE_TABLE: &str = "
    DEFINE TABLE IF NOT EXISTS __schema_file SCHEMAFULL PERMISSIONS NONE;
    DEFINE FIELD IF NOT EXISTS hash ON __schema_file TYPE string;
    DEFINE FIELD IF NOT EXISTS applied_at ON __schema_file TYPE datetime;
";

#[derive(SurrealValue)]
struct Applied {
    path: String,
    hash: String,
}

/// Applies the files whose text or parameters changed since the last run and returns their paths
pub async fn apply_schema(
    db: &Surreal<Any>,
    files: &[SchemaFile],
    params: &[(&str, &str)],
) -> Result<Vec<&'static str>, Error> {
    db.query(SCHEMA_FILE_TABLE).await?.check()?;
    let mut ran = Vec::new();
    for file in changed_schema(db, files, params).await? {
        let mut query = db.query(file.sql);
        for (name, value) in params {
            query = query.bind((name.to_string(), value.to_string()));
        }
        query
            .await
            .and_then(|response| response.check())
            .map_err(|source| Error::Schema {
                path: file.path.to_owned(),
                source: Box::new(source),
            })?;
        db
            .query("UPSERT type::record('__schema_file', $path) SET hash = $hash, applied_at = time::now()")
            .bind(("path", file.path))
            .bind(("hash", hash(file.sql, params)))
            .await?
            .check()?;
        ran.push(file.path);
    }
    Ok(ran)
}

/// The files whose text or parameters changed since they were last applied
pub async fn changed_schema(
    db: &Surreal<Any>,
    files: &[SchemaFile],
    params: &[(&str, &str)],
) -> Result<Vec<SchemaFile>, Error> {
    // unpaged: the table holds one row per schema file
    let applied: Vec<Applied> = db
        .query("SELECT record::id(id) AS path, hash FROM __schema_file")
        .await?
        .check()?
        .take(0)?;
    let changed = files
        .iter()
        .filter(|file| {
            let hash = hash(file.sql, params);
            !applied
                .iter()
                .any(|a| a.path == file.path && a.hash == hash)
        })
        .copied()
        .collect();
    Ok(changed)
}

/// The server's refusal of `file`, found by running it in a transaction that is then cancelled
pub async fn dry_run_schema(
    db: &Surreal<Any>,
    file: &SchemaFile,
    params: &[(&str, &str)],
) -> Result<Option<surrealdb::Error>, Error> {
    let mut query = db.query("BEGIN").query(file.sql).query("CANCEL");
    for (name, value) in params {
        query = query.bind((name.to_string(), value.to_string()));
    }
    let mut response = query.await?;
    // a cancelled transaction reports its accepted statements as cancelled or not executed
    let refusal = response
        .take_errors()
        .into_iter()
        .filter(|(_, error)| {
            !matches!(
                error.query_details(),
                Some(QueryError::Cancelled | QueryError::NotExecuted)
            )
        })
        .min_by_key(|(index, _)| *index)
        .map(|(_, error)| error);
    Ok(refusal)
}

fn hash(sql: &str, params: &[(&str, &str)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql);
    for (name, value) in params {
        hasher.update([0]);
        hasher.update(name);
        hasher.update("=");
        hasher.update(value);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{connect, new_tenant};

    const A: SchemaFile = SchemaFile {
        path: "a.surql",
        sql: "DEFINE TABLE OVERWRITE a SCHEMAFULL;",
    };
    const A_CHANGED: SchemaFile = SchemaFile {
        path: "a.surql",
        sql: "DEFINE TABLE OVERWRITE a SCHEMAFULL; DEFINE FIELD OVERWRITE n ON a TYPE int;",
    };
    const A_OPTIONAL: SchemaFile = SchemaFile {
        path: "a.surql",
        sql: "DEFINE TABLE OVERWRITE a SCHEMAFULL; DEFINE FIELD OVERWRITE n ON a TYPE option<int>;",
    };
    const B: SchemaFile = SchemaFile {
        path: "b.surql",
        sql: "DEFINE TABLE OVERWRITE b SCHEMAFULL;",
    };
    const PARAM: SchemaFile = SchemaFile {
        path: "param.surql",
        sql: "DEFINE PARAM OVERWRITE $answer VALUE $value;",
    };
    const BROKEN: SchemaFile = SchemaFile {
        path: "c.surql",
        sql: "DEFINE TABLE c TYPE NOPE;",
    };
    const FIXED: SchemaFile = SchemaFile {
        path: "c.surql",
        sql: "DEFINE TABLE OVERWRITE c SCHEMAFULL;",
    };

    async fn tenant_session() -> Surreal<Any> {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        db.system_session(&tenant).await.unwrap()
    }

    #[tokio::test]
    async fn only_new_or_changed_files_run() {
        let session = tenant_session().await;
        let run = |files| apply_schema(&session, files, &[]);

        assert_eq!(run(&[A, B]).await.unwrap(), ["a.surql", "b.surql"]);
        assert!(run(&[A, B]).await.unwrap().is_empty());
        assert_eq!(run(&[A_CHANGED, B]).await.unwrap(), ["a.surql"]);
    }

    #[tokio::test]
    async fn a_file_runs_again_when_its_parameters_change() {
        let session = tenant_session().await;
        let one = [("value", "1")];
        let two = [("value", "2")];

        let ran = apply_schema(&session, &[PARAM], &one).await.unwrap();
        assert_eq!(ran, ["param.surql"]);
        assert!(
            apply_schema(&session, &[PARAM], &one)
                .await
                .unwrap()
                .is_empty()
        );
        let ran = apply_schema(&session, &[PARAM], &two).await.unwrap();
        assert_eq!(ran, ["param.surql"]);
    }

    #[tokio::test]
    async fn a_rejected_file_is_not_recorded_and_runs_next_time() {
        let session = tenant_session().await;

        for _ in 0..2 {
            let error = apply_schema(&session, &[A, BROKEN], &[]).await.unwrap_err();
            assert!(
                matches!(&error, Error::Schema { path, .. } if path == "c.surql"),
                "{error:?}"
            );
        }
        let ran = apply_schema(&session, &[A, FIXED], &[]).await.unwrap();
        assert_eq!(ran, ["c.surql"]);
    }

    #[tokio::test]
    async fn a_dry_run_reports_only_a_refusal_and_applies_nothing() {
        let session = tenant_session().await;
        apply_schema(&session, &[A], &[]).await.unwrap();
        session.query("CREATE a").await.unwrap().check().unwrap();

        // a required field on a table with rows
        let refusal = dry_run_schema(&session, &A_CHANGED, &[]).await.unwrap();
        assert!(refusal.is_some(), "accepted {}", A_CHANGED.sql);
        let refusal = dry_run_schema(&session, &A_OPTIONAL, &[]).await.unwrap();
        assert!(refusal.is_none(), "{refusal:?}");

        let fields: Vec<String> = session
            .query("RETURN object::keys((INFO FOR TABLE a).fields)")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(fields.is_empty(), "{fields:?}");
    }
}
