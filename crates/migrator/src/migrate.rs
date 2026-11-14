use std::error::Error as StdError;

use resysto_surrealdb::{
    SchemaFile, apply_schema, changed_schema, dry_run_schema, write_in_batches,
};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::SurrealValue;

use crate::manifest::{self, Manifest, Remove, Step};
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

#[derive(SurrealValue)]
struct Failed {
    id: i64,
    error: Option<String>,
}

// what migrate does, in order
struct Work {
    files: Vec<SchemaFile>,
    pending: Vec<Manifest>,
}

/// What `migrate` would do on a database, found without changing it
#[derive(Debug)]
pub(crate) struct Plan {
    /// Each changed schema file that runs before the manifests, with the server's refusal
    pub(crate) files: Vec<(&'static str, Option<String>)>,
    /// Each pending manifest, with the error of its last run
    pub(crate) manifests: Vec<(&'static str, Option<String>)>,
}

/// What `migrate` applied on a database
#[derive(Debug)]
pub(crate) struct Applied {
    /// The changed schema files, applied before the manifests
    pub(crate) files: Vec<&'static str>,
    /// The manifests that ran
    pub(crate) manifests: Vec<&'static str>,
}

/// Applies the schema files, then the pending manifests; a failed one restarts from step one
pub(crate) async fn migrate(
    db: &Surreal<Any>,
    lane: &Lane,
    params: &[(&str, &str)],
) -> Result<Applied, Error> {
    db.query(MIGRATION_TABLE).await?.check()?;
    let Work { files, pending } = work(lane, &done(db).await?)?;
    let files = apply_schema(db, &files, params).await?;
    let mut manifests = Vec::with_capacity(pending.len());
    for manifest in &pending {
        apply(db, lane, manifest, params).await?;
        manifests.push(manifest.path);
    }
    Ok(Applied { files, manifests })
}

/// Takes the decisions of `migrate` and tries each changed file, applying nothing
pub(crate) async fn plan(
    db: &Surreal<Any>,
    lane: &Lane,
    params: &[(&str, &str)],
) -> Result<Plan, Error> {
    let Work { files, pending } = work(lane, &done(db).await?)?;
    let mut checked = Vec::new();
    for file in changed_schema(db, &files, params).await? {
        let refusal = dry_run_schema(db, &file, params).await?;
        checked.push((file.path, refusal.map(|error| error.to_string())));
    }
    let failed: Vec<Failed> = db
        .query("SELECT record::id(id) AS id, error FROM __migration WHERE phase = 'failed'")
        .await?
        .check()?
        .take(0)?;
    let manifests = pending
        .iter()
        .map(|manifest| {
            let error = failed
                .iter()
                .find(|row| row.id == manifest.id)
                .and_then(|row| row.error.clone());
            (manifest.path, error)
        })
        .collect();
    Ok(Plan {
        files: checked,
        manifests,
    })
}

/// What `migrate` would do on a database that doesn't exist yet
pub(crate) fn plan_new(lane: &Lane) -> Result<Plan, Error> {
    let Work { files, pending } = work(lane, &[])?;
    Ok(Plan {
        files: files.iter().map(|file| (file.path, None)).collect(),
        manifests: pending
            .iter()
            .map(|manifest| (manifest.path, None))
            .collect(),
    })
}

async fn done(db: &Surreal<Any>) -> Result<Vec<Done>, Error> {
    let done: Vec<Done> = db
        .query("SELECT record::id(id) AS id, hash FROM __migration WHERE phase = 'done'")
        .await?
        .check()?
        .take(0)?;
    Ok(done)
}

fn work(lane: &Lane, done: &[Done]) -> Result<Work, Error> {
    check_schema(lane)?;
    let newest_done = done.iter().map(|row| row.id).max();
    let mut pending = Vec::new();
    for manifest in manifest::load(lane)? {
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
    // files cited by a pending manifest are applied by its schema step
    let files = lane
        .schema
        .iter()
        .filter(|file| !pending.iter().any(|manifest| manifest.cites(file.path)))
        .copied()
        .collect();
    Ok(Work { files, pending })
}

// a background UNIQUE build reports success, then rejects writes to its table on a duplicate
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
        Step::Remove(Remove(sql)) | Step::Migrate(sql) => {
            db.query(sql.as_str()).await?.check()?;
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

    const PERSON_OPTIONAL_EMAIL: SchemaFile = SchemaFile {
        path: "person.surql",
        sql: "DEFINE TABLE OVERWRITE person SCHEMAFULL;
              DEFINE FIELD OVERWRITE email ON person TYPE option<string>;
              DEFINE INDEX OVERWRITE person_email ON person FIELDS email;",
    };
    const PERSON_REQUIRED_EMAIL: SchemaFile = SchemaFile {
        path: "person.surql",
        sql: "DEFINE TABLE OVERWRITE person SCHEMAFULL;
              DEFINE FIELD OVERWRITE email ON person TYPE string;
              DEFINE INDEX OVERWRITE person_email ON person FIELDS email;",
    };
    // the server refuses to migrate a field that an index reads
    const REQUIRE_EMAIL: &str = r#"
        title = "person.email required"

        [[contract]]
        remove = { table = "person", index = "person_email" }

        [[contract]]
        migrate = """
            DEFINE FIELD OVERWRITE email ON person TYPE string
                MIGRATE EXISTING ROWS AS email_required USING $value ?? '' FALLBACK ''
        """

        [[contract]]
        schema = "person.surql"
    "#;
    const REQUIRE_EMAIL_PATH: &str = "20261114100000-person-email-required.toml";

    #[derive(Debug, PartialEq, SurrealValue)]
    struct Row {
        phase: String,
        error: Option<String>,
        updated_at: Datetime,
    }

    async fn tenant() -> Surreal<Any> {
        let db = connect().await;
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        db.define_database(&name).await.unwrap();
        db.system_session(&name).await.unwrap()
    }

    async fn people(emails: &[String]) -> Surreal<Any> {
        let session = tenant().await;
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

    // the names under `fields` or `indexes` of INFO FOR TABLE person
    async fn defined(db: &Surreal<Any>, kind: &str) -> Vec<String> {
        let info: Value = db
            .query("INFO FOR TABLE person")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let Value::Object(info) = info else {
            panic!("{info:?}")
        };
        let Some(Value::Object(names)) = info.get(kind) else {
            panic!("{info:?}")
        };
        names.keys().cloned().collect()
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
        // batches of 500, 500 and 200
        let emails: Vec<String> = (0..1200).map(|n| format!("P{n}@X.IT")).collect();
        let db = people(&emails).await;

        let applied = migrate(&db, &lane(&[(MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[])
            .await
            .unwrap();

        assert!(applied.files.is_empty(), "{applied:?}");
        assert_eq!(applied.manifests, [MOVE_EMAIL_PATH]);
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
        assert_eq!(defined(&db, "fields").await, ["email"]);
        let done = row(&db).await;
        assert_eq!((done.phase.as_str(), &done.error), ("done", &None));

        let applied = migrate(&db, &lane(&[(MOVE_EMAIL_PATH, MOVE_EMAIL)]), &[])
            .await
            .unwrap();
        assert!(
            applied.files.is_empty() && applied.manifests.is_empty(),
            "{applied:?}"
        );
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
            defined(&db, "fields").await,
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
        assert_eq!(defined(&db, "fields").await, ["email"]);
    }

    #[tokio::test]
    async fn a_migrate_step_makes_an_indexed_field_of_a_populated_table_required() {
        let db = tenant().await;
        let before = Lane {
            schema: &[PERSON_OPTIONAL_EMAIL],
            manifests: &[],
        };
        migrate(&db, &before, &[]).await.unwrap();
        db.query("CREATE person SET email = 'a@x.it'; CREATE person;")
            .await
            .unwrap()
            .check()
            .unwrap();

        let after = Lane {
            schema: &[PERSON_REQUIRED_EMAIL],
            manifests: &[ManifestFile {
                path: REQUIRE_EMAIL_PATH,
                toml: REQUIRE_EMAIL,
            }],
        };
        migrate(&db, &after, &[]).await.unwrap();

        let emails: Vec<String> = db
            .query("SELECT VALUE email FROM person ORDER BY email")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(emails, ["", "a@x.it"]);
        assert_eq!(defined(&db, "indexes").await, ["person_email"]);
    }

    #[tokio::test]
    async fn a_plan_lists_changed_files_with_their_refusal_and_pending_manifests_with_their_error()
    {
        let db = tenant().await;
        let before = Lane {
            schema: &[PERSON_OPTIONAL_EMAIL],
            manifests: &[],
        };
        migrate(&db, &before, &[]).await.unwrap();
        db.query("CREATE person").await.unwrap().check().unwrap();

        let planned = plan(&db, &before, &[]).await.unwrap();
        assert!(
            planned.files.is_empty() && planned.manifests.is_empty(),
            "{planned:?}"
        );

        let forgotten = Lane {
            schema: &[PERSON_REQUIRED_EMAIL],
            manifests: &[],
        };
        let planned = plan(&db, &forgotten, &[]).await.unwrap();
        assert!(
            matches!(planned.files.as_slice(), [("person.surql", Some(_))])
                && planned.manifests.is_empty(),
            "{planned:?}"
        );

        let with_manifest = Lane {
            schema: &[PERSON_REQUIRED_EMAIL],
            manifests: &[ManifestFile {
                path: REQUIRE_EMAIL_PATH,
                toml: REQUIRE_EMAIL,
            }],
        };
        let planned = plan(&db, &with_manifest, &[]).await.unwrap();
        assert!(
            planned.files.is_empty(),
            "the cited file was tried: {planned:?}"
        );
        assert_eq!(planned.manifests, [(REQUIRE_EMAIL_PATH, None)]);

        let failing = Lane {
            schema: &[PERSON_OPTIONAL_EMAIL],
            manifests: &[ManifestFile {
                path: "20261114110000-failing.toml",
                toml: "title = \"failing\"\n[[expand]]\nassert = \"RETURN false\"",
            }],
        };
        migrate(&db, &failing, &[]).await.unwrap_err();
        let planned = plan(&db, &failing, &[]).await.unwrap();
        assert!(
            matches!(
                planned.manifests.as_slice(),
                [("20261114110000-failing.toml", Some(_))]
            ),
            "{planned:?}"
        );
    }
}
