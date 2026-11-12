//! The `.surql` files of each lane, embedded in the binary, and the provisioning of databases.

use resysto_surrealdb::{Db, Error, SchemaFile, apply_schema};

const MASTER_DB: &str = "__master";

// Every file under `migrations/<lane>/schema/`, in the order they apply.
const MASTER: &[SchemaFile] = &[];

const TENANT: &[SchemaFile] = &[SchemaFile {
    path: "access.surql",
    sql: include_str!("../../../migrations/tenant/schema/access.surql"),
}];

/// Creates the master database if missing and applies the `master` lane. Safe to repeat.
pub async fn provision_master(db: &Db) -> Result<(), Error> {
    db.define_database(MASTER_DB).await?;
    let session = db.system_session(MASTER_DB).await?;
    apply_schema(&session, MASTER, &[]).await?;
    Ok(())
}

/// Creates `database` if missing and applies the `tenant` lane. Safe to repeat.
pub async fn provision_tenant(db: &Db, database: &str) -> Result<(), Error> {
    db.define_database(database).await?;
    let session = db.system_session(database).await?;
    apply_schema(&session, TENANT, &[("access_key", db.verifying_key())]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::ErrorKind;
    use std::path::Path;

    use resysto_surrealdb::Config;
    use surrealdb::types::{RecordId, Uuid, Value};

    use super::*;

    // Test-only key pair.
    const SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgCcu34KTFnm0J5qWJ
AxhAcHSbqFj+B/am0htHlDufqWihRANCAASN/BWdDI7GaVVD6HAh20C4ZzJSHSsz
9htiM2MVKHWJFnxAy4GKb6YFXGV6KEDsrn5MQIa7TTsyRDnIcCjQztXz
-----END PRIVATE KEY-----
";
    const VERIFYING_KEY: &str = "-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEjfwVnQyOxmlVQ+hwIdtAuGcyUh0r
M/YbYjNjFSh1iRZ8QMuBim+mBVxleihA7K5+TECGu007MkQ5yHAo0M7V8w==
-----END PUBLIC KEY-----
";

    async fn connect() -> Db {
        let port = std::env::var("SURREAL_PORT").unwrap_or_else(|_| "18000".into());
        Db::connect(Config {
            endpoint: format!("ws://127.0.0.1:{port}"),
            namespace: "resysto-dev".into(),
            username: "ns_migrator".into(),
            password: "migrator".into(),
            signing_key: SIGNING_KEY.into(),
            verifying_key: VERIFYING_KEY.into(),
        })
        .await
        .unwrap()
    }

    async fn new_tenant(db: &Db) -> String {
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        provision_tenant(db, &name).await.unwrap();
        name
    }

    async fn info(db: &Db, database: &str) -> Value {
        let session = db.system_session(database).await.unwrap();
        session.query("INFO FOR DB").await.unwrap().take(0).unwrap()
    }

    // Path inside `schema/`, with `/` separators, mapped to the file's text.
    fn surql_files(root: &Path, dir: &Path, found: &mut BTreeMap<String, String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return,
            Err(error) => panic!("{}: {error}", dir.display()),
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                surql_files(root, &path, found);
            } else if path.extension().is_some_and(|ext| ext == "surql") {
                let key = path.strip_prefix(root).unwrap().to_string_lossy();
                let text = std::fs::read_to_string(&path).unwrap();
                found.insert(key.replace('\\', "/"), text);
            }
        }
    }

    #[test]
    fn every_schema_file_is_listed_once_with_its_text() {
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        for (lane, files) in [("master", MASTER), ("tenant", TENANT)] {
            let dir = migrations.join(lane).join("schema");
            let mut on_disk = BTreeMap::new();
            surql_files(&dir, &dir, &mut on_disk);
            let listed: BTreeMap<String, String> = files
                .iter()
                .map(|file| (file.path.to_owned(), file.sql.to_owned()))
                .collect();
            assert_eq!(listed.len(), files.len(), "{lane}: a path is listed twice");
            assert_eq!(listed, on_disk, "{lane}");
        }
    }

    #[tokio::test]
    async fn a_provisioned_tenant_accepts_record_sessions() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let user = RecordId::new("account", Uuid::new_v7());

        let session = db.record_session(&tenant, user.clone()).await.unwrap();

        let auth: Option<RecordId> = session
            .query("RETURN $auth")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(auth, Some(user));
    }

    #[tokio::test]
    async fn provisioning_again_leaves_the_schema_as_it_was() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let before = info(&db, &tenant).await;
        provision_tenant(&db, &tenant).await.unwrap();
        assert_eq!(info(&db, &tenant).await, before);
    }
}
