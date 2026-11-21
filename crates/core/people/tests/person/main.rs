#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod router;
mod tools;

use resysto_surrealdb::{Db, Grant};

const TENANT: &str = "t_acme";

async fn tenant() -> Db {
    let db = Db::memory().await.unwrap();
    resysto_migrator::provision_tenant(&db, TENANT)
        .await
        .unwrap();
    db
}

fn everything() -> Grant {
    Grant {
        organizations: None,
        write: true,
    }
}
