use std::error::Error as StdError;

use resysto_surrealdb::{SchemaFile, apply_schema, write_in_batches};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::SurrealValue;

use crate::manifest::{self, Manifest, Step};
use crate::{Error, Lane};

const MIGRATION_TABLE: &str = "
    DEFINE TABLE IF NOT EXISTS __migration SCHEMAFULL PERMISSIONS NONE;
    DEFINE FIELD IF NOT EXISTS title ON __migration TYPE string;
    DEFINE FIELD IF NOT EXISTS hash ON __migration TYPE string;
    DEFINE FIELD IF NOT EXISTS phase ON __migration TYPE 'failed' | 'done';
    DEFINE FIELD IF NOT EXISTS error ON __migration TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS updated_at ON __migration TYPE datetime;
";

#[derive(SurrealValue)]
struct Done {
    id: i64,
    hash: String,
}

/// Applies the schema files, then each pending manifest. A failed manifest is retried from its
/// first step on the next run.
pub(crate) async fn migrate(
    db: &Surreal<Any>,
    lane: &Lane,
    params: &[(&str, &str)],
) -> Result<(), Error> {
    check_schema(lane)?;
    let manifests = manifest::load(lane)?;
    db.query(MIGRATION_TABLE).await?.check()?;
    let done: Vec<Done> = db
        .query("SELECT record::id(id) AS id, hash FROM __migration WHERE phase = 'done'")
        .await?
        .check()?
        .take(0)?;
    let newest_done = done.iter().map(|row| row.id).max();
    let mut pending = Vec::new();
    for manifest in &manifests {
        match done.iter().find(|row| row.id == manifest.id) {
            Some(row) if row.hash != manifest.hash => {
                return Err(Error::ManifestChanged {
                    path: manifest.path,
                });
            }
            Some(_) => {}
            None if newest_done.is_some_and(|newest| manifest.id < newest) => {
                return Err(Error::OutOfOrder {
                    path: manifest.path,
                });
            }
            None => pending.push(manifest),
        }
    }
    // Files cited by a pending manifest are applied by its schema step.
    let files: Vec<SchemaFile> = lane
        .schema
        .iter()
        .filter(|file| !pending.iter().any(|manifest| manifest.cites(file.path)))
        .copied()
        .collect();
    apply_schema(db, &files, params).await?;
    for manifest in pending {
        apply(db, lane, manifest, params).await?;
    }
    Ok(())
}

// A background UNIQUE build reports success, then rejects writes to its table on a duplicate.
pub(crate) fn check_schema(lane: &Lane) -> Result<(), Error> {
    let concurrent = lane.schema.iter().find(|file| {
        file.sql
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|word| word.eq_ignore_ascii_case("concurrently"))
    });
    match concurrent {
        Some(file) => Err(Error::ConcurrentIndex { path: file.path }),
        None => Ok(()),
    }
}

async fn apply(
    db: &Surreal<Any>,
    lane: &Lane,
    manifest: &Manifest,
    params: &[(&str, &str)],
) -> Result<(), Error> {
    let result = match run(db, lane, manifest, "expand", &manifest.expand, params).await {
        Ok(()) => run(db, lane, manifest, "contract", &manifest.contract, params).await,
        failed => failed,
    };
    let (phase, error) = match &result {
        Ok(()) => ("done", None),
        Err(error) => ("failed", Some(chain(error))),
    };
    db.query(
        "UPSERT type::record('__migration', $id)
         SET title = $title, hash = $hash, phase = $phase, error = $error, updated_at = time::now()",
    )
    .bind(("id", manifest.id))
    .bind(("title", manifest.title.clone()))
    .bind(("hash", manifest.hash.clone()))
    .bind(("phase", phase))
    .bind(("error", error))
    .await?
    .check()?;
    result
}

async fn run(
    db: &Surreal<Any>,
    lane: &Lane,
    manifest: &Manifest,
    section: &'static str,
    steps: &[Step],
    params: &[(&str, &str)],
) -> Result<(), Error> {
    for (step, n) in steps.iter().zip(1..) {
        run_step(db, lane, step, params)
            .await
            .map_err(|source| Error::Step {
                path: manifest.path,
                section,
                step: n,
                source,
            })?;
    }
    Ok(())
}

async fn run_step(
    db: &Surreal<Any>,
    lane: &Lane,
    step: &Step,
    params: &[(&str, &str)],
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    match step {
        Step::Schema(path) => {
            let file: Vec<SchemaFile> = lane
                .schema
                .iter()
                .filter(|file| file.path == path)
                .copied()
                .collect();
            apply_schema(db, &file, params).await?;
        }
        Step::Data(data) => {
            write_in_batches(data.batch, || db.query(data.sql.as_str())).await?;
        }
        Step::Assert(sql) => {
            let mut response = db.query(sql.as_str()).await?.check()?;
            let last = response.num_statements().saturating_sub(1);
            let passed: Option<bool> = response.take(last)?;
            if passed != Some(true) {
                return Err(format!("assert returned {passed:?}").into());
            }
        }
        Step::Remove(remove) => {
            db.query(remove.0.as_str()).await?.check()?;
        }
    }
    Ok(())
}

pub(crate) fn chain(error: &Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use surrealdb::types::{Datetime, Uuid, Value};

    use super::*;
    use crate::ManifestFile;
    use crate::tests::connect;

    const PERSON_BEFORE: SchemaFile = SchemaFile {
        path: "person.surql",
        sql: "DEFINE TABLE OVERWRITE person SCHEMAFULL;
              DEFINE FIELD OVERWRITE email_old ON person TYPE option<string>;",
    };
    const PERSON: SchemaFile = SchemaFile {
        path: "person.surql",
        sql: "DEFINE TABLE OVERWRITE person SCHEMAFULL;
              DEFINE FIELD OVERWRITE email ON person TYPE option<string>;",
    };
    const MOVE_EMAIL: &str = r#"
        title = "person.email from email_old"

        [[expand]]
        assert = "RETURN count(SELECT VALUE id FROM person WHERE !string::contains(email_old, '@') LIMIT 1) = 0"

        [[expand]]
        schema = "person.surql"

        [[expand]]
        data = { sql = """
            LET $batch = (SELECT VALUE id FROM type::record('person', $range)
                          WHERE email = NONE LIMIT $limit);
            UPDATE $batch SET email = string::lowercase(email_old) RETURN NONE;
            RETURN $batch;
        """, batch = 500 }

        [[contract]]
        assert = "RETURN count(SELECT VALUE id FROM person WHERE email = NONE LIMIT 1) = 0"

        [[contract]]
        remove = { table = "person", field = "email_old" }
    "#;
    const MOVE_EMAIL_PATH: &str = "20261112143000-person-email.toml";
    const MOVE_EMAIL_ID: i64 = 20261112143000;

    #[derive(Debug, PartialEq, SurrealValue)]
    struct Row {
        phase: String,
        error: Option<String>,
        updated_at: Datetime,
    }

    async fn people(emails: &[String]) -> Surreal<Any> {
        let db = connect().await;
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        db.define_database(&name).await.unwrap();
        let session = db.system_session(&name).await.unwrap();
        let before = Lane {
            schema: &[PERSON_BEFORE],
            manifests: &[],
        };
        migrate(&session, &before, &[]).await.unwrap();
        session
            .query("FOR $email IN $emails { CREATE type::record('person', rand::uuid::v7()) SET email_old = $email; }")
            .bind(("emails", emails.to_vec()))
            .await
            .unwrap()
            .check()
            .unwrap();
        session
    }

    fn lane(manifests: &[(&'static str, &'static str)]) -> Lane {
        let manifests = manifests
            .iter()
            .map(|&(path, toml)| ManifestFile { path, toml })
            .collect();
        Lane {
            schema: &[PERSON],
            manifests: Vec::leak(manifests),
        }
    }

    async fn row(db: &Surreal<Any>) -> Row {
        let row: Option<Row> = db
            .query("SELECT phase, error, updated_at FROM ONLY type::record('__migration', $id)")
            .bind(("id", MOVE_EMAIL_ID))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        row.unwrap()
    }

    async fn fields(db: &Surreal<Any>) -> Vec<String> {
        let info: Value = db
            .query("INFO FOR TABLE person")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let Value::Object(info) = info else {
            panic!("{info:?}")
        };
        let Some(Value::Object(fields)) = info.get("fields") else {
            panic!("{info:?}")
        };
        fields.keys().cloned().collect()
    }

    #[test]
    fn a_schema_file_with_a_concurrent_index_is_rejected() {
        let check = |sql: &'static str| {
            check_schema(&Lane {
                schema: Vec::leak(vec![SchemaFile {
                    path: "person.surql",
                    sql,
                }]),
                manifests: &[],
            })
        };

        for sql in [
            "DEFINE INDEX OVERWRITE person_n ON person FIELDS n CONCURRENTLY;",
            "define index person_n on person fields n concurrently;",
        ] {
            let result = check(sql);
            assert!(
                matches!(
                    result,
                    Err(Error::ConcurrentIndex {
                        path: "person.surql"
                    })
                ),
                "accepted {sql}"
            );
        }
        check("DEFINE FIELD OVERWRITE ran_concurrently ON person TYPE bool;").unwrap();
    }

    #[tokio::test]
    async fn a_manifest_runs_once_and_then_neither_changes_nor_gets_an_older_one_before_it() {
        // Batches of 500, 500 and 200.
        let emails: Vec<String> = (0..1200).map(|n| format!("P{n}@X.IT")).collect();
        let db = people(&emails).await;

        migrate(&db, &lane(&[(MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[])
            .await
            .unwrap();

        let mut moved: Vec<Option<String>> = db
            .query("SELECT VALUE email FROM person")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let mut expected: Vec<Option<String>> = emails
            .iter()
            .map(|email| Some(email.to_lowercase()))
            .collect();
        moved.sort();
        expected.sort();
        assert_eq!(moved, expected);
        assert_eq!(fields(&db).await, ["email"]);
        let done = row(&db).await;
        assert_eq!((done.phase.as_str(), &done.error), ("done", &None));

        migrate(&db, &lane(&[(MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[])
            .await
            .unwrap();
        assert_eq!(row(&db).await, done, "the manifest ran again");

        let edited = MOVE_EMAIL.replace("batch = 500", "batch = 400").leak();
        let result = migrate(&db, &lane(&[(MOVE_EMAIL_PATH, edited)]), &[]).await;
        assert!(
            matches!(
                result,
                Err(Error::ManifestChanged {
                    path: MOVE_EMAIL_PATH
                })
            ),
            "{result:?}"
        );

        let older = (
            "20261112120000-older.toml",
            "title = \"older\"\n[[expand]]\nassert = \"RETURN true\"",
        );
        let result = migrate(&db, &lane(&[older, (MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[]).await;
        assert!(
            matches!(result, Err(Error::OutOfOrder { path }) if path == older.0),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_manifest_keeps_its_error_and_its_schema_file_until_it_runs_again() {
        let db = people(&["A@X.IT".into(), "broken".into()]).await;

        let result = migrate(&db, &lane(&[(MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[]).await;

        assert!(
            matches!(
                result,
                Err(Error::Step {
                    path: MOVE_EMAIL_PATH,
                    section: "expand",
                    step: 1,
                    ..
                })
            ),
            "{result:?}"
        );
        let failed = row(&db).await;
        assert_eq!(failed.phase, "failed");
        assert!(failed.error.is_some());
        assert_eq!(
            fields(&db).await,
            ["email_old"],
            "person.surql ran before its manifest"
        );

        db.query("UPDATE person SET email_old = 'b@x.it' WHERE email_old = 'broken'")
            .await
            .unwrap()
            .check()
            .unwrap();
        migrate(&db, &lane(&[(MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[])
            .await
            .unwrap();

        let done = row(&db).await;
        assert_eq!((done.phase.as_str(), &done.error), ("done", &None));
        assert_eq!(fields(&db).await, ["email"]);
    }
}
