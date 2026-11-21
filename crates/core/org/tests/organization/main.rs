#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod router;
mod tools;

use resysto_surrealdb::Db;

const TENANT: &str = "t_acme";

// a tenant plus a table that tags its rows like every bounded context does
async fn tenant() -> Db {
    let db = Db::memory().await.unwrap();
    resysto_migrator::provision_tenant(&db, TENANT)
        .await
        .unwrap();
    db.system_session(TENANT)
        .await
        .unwrap()
        .query(
            "DEFINE TABLE item SCHEMAFULL PERMISSIONS FULL;
             DEFINE FIELD organization_ids ON item TYPE array<record<organization>>
                 REFERENCE ON DELETE REJECT ASSERT array::len($value) > 0;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
    db
}
