//! The SurrealQL of organizations, one function per query

use std::collections::BTreeMap;

use resysto_model::OrganizationId;
use resysto_surrealdb::{Error, Executor, Page, PageRequest, page, write_in_batches};
use surrealdb::types::SurrealValue;

use super::model::{Organization, OrganizationRecord};

/// Records a reassign rewrites per transaction
pub(super) const REASSIGN_BATCH: u32 = 500;

#[derive(SurrealValue)]
struct Use {
    table: String,
    count: i64,
}

pub(super) async fn find(
    db: &impl Executor,
    id: OrganizationId,
) -> Result<Option<OrganizationRecord>, Error> {
    let found: Option<OrganizationRecord> = db
        .query("SELECT * FROM ONLY $id")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(found)
}

const LIST: &str = "SELECT * FROM organization WHERE id < type::record('organization', $before)
    ORDER BY id DESC LIMIT $limit";

/// One page of the organizations, newest first
pub(super) async fn list(
    db: &impl Executor,
    request: PageRequest,
) -> Result<Page<OrganizationRecord>, Error> {
    let query = db.query(LIST);
    page(query, request).await
}

/// The ancestors of an organization, or `None` when it does not exist
pub(super) async fn path(
    db: &impl Executor,
    id: OrganizationId,
) -> Result<Option<Vec<OrganizationId>>, Error> {
    // one path per organization found; an Option would take the path's first element
    let paths: Vec<Vec<OrganizationId>> = db
        .query("SELECT VALUE path FROM $id")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(paths.into_iter().next())
}

pub(super) async fn insert(
    db: &impl Executor,
    id: OrganizationId,
    organization: Organization,
) -> Result<Option<OrganizationRecord>, Error> {
    let created: Option<OrganizationRecord> = db
        .query("CREATE ONLY $id CONTENT $organization")
        .bind(("id", id))
        .bind(("organization", organization))
        .await?
        .check()?
        .take(0)?;
    Ok(created)
}

/// Writes every field of `organization`, a `None` removes the field; the database keeps the rest
pub(super) async fn update(
    db: &impl Executor,
    id: OrganizationId,
    organization: Organization,
) -> Result<Option<OrganizationRecord>, Error> {
    let updated: Option<OrganizationRecord> = db
        .query("UPDATE ONLY $id MERGE $organization")
        .bind(("id", id))
        .bind(("organization", organization))
        .await?
        .check()?
        .take(0)?;
    Ok(updated)
}

/// How many ancestors each organization below `id` has
pub(super) async fn depths_below(
    db: &impl Executor,
    id: OrganizationId,
) -> Result<Vec<i64>, Error> {
    let depths: Vec<i64> = db
        .query("SELECT VALUE array::len(path) FROM organization WHERE path CONTAINS $id")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(depths)
}

/// Rewrites the paths below `id` after it moved from under `old` to under `new`
pub(super) async fn rehome_branch(
    db: &impl Executor,
    id: OrganizationId,
    old: Vec<OrganizationId>,
    new: Vec<OrganizationId>,
) -> Result<(), Error> {
    db.query(
        "UPDATE organization
         SET path = array::concat($new, [$id], array::slice(path, array::len($old) + 1))
         WHERE path CONTAINS $id
         RETURN NONE",
    )
    .bind(("id", id))
    .bind(("old", old))
    .bind(("new", new))
    .await?
    .check()?;
    Ok(())
}

pub(super) async fn exists(db: &impl Executor, id: OrganizationId) -> Result<bool, Error> {
    let exists: Option<bool> = db
        .query("RETURN record::exists($id)")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(exists == Some(true))
}

pub(super) async fn has_children(db: &impl Executor, id: OrganizationId) -> Result<bool, Error> {
    let children: Option<i64> = db
        .query("RETURN count((SELECT VALUE id FROM organization WHERE parent = $id LIMIT 1))")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(children.unwrap_or_default() > 0)
}

/// How many records of each table still carry the organization
pub(super) async fn uses(
    db: &impl Executor,
    id: OrganizationId,
) -> Result<BTreeMap<String, i64>, Error> {
    let rows: Vec<Use> = db
        .query(
            "SELECT table, count() AS count
             FROM (SELECT VALUE <~? FROM ONLY $id).map(|$record| { table: record::tb($record) })
             GROUP BY table",
        )
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(rows.into_iter().map(|row| (row.table, row.count)).collect())
}

/// Whether the organization went; ON DELETE REJECT refuses it while a record carries it
pub(super) async fn delete(db: &impl Executor, id: OrganizationId) -> Result<bool, Error> {
    let deleted: Option<OrganizationId> = db
        .query("DELETE ONLY $id RETURN VALUE $before.id")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(deleted.is_some())
}

pub(super) async fn below(
    db: &impl Executor,
    organizations: &[OrganizationId],
) -> Result<Vec<OrganizationId>, Error> {
    let below: Vec<OrganizationId> = db
        .query("SELECT VALUE id FROM organization WHERE path CONTAINSANY $organizations")
        .bind(("organizations", organizations.to_vec()))
        .await?
        .check()?
        .take(0)?;
    Ok(below)
}

/// Replaces `from` with `to` in every record, a batch per transaction, and returns how many
pub(super) async fn reassign(
    db: &impl Executor,
    from: OrganizationId,
    to: OrganizationId,
) -> Result<u64, Error> {
    // every reference to an organization is an organization_ids field, and a rewritten record
    // stops referencing `from`, so the next batch starts from what is left and `$range` is unused;
    // the limit is a literal because 4.0.0 reads every reference when it is a parameter
    write_in_batches(REASSIGN_BATCH, || {
        db.query(format!(
            "LET $batch = (SELECT VALUE <~(? LIMIT {REASSIGN_BATCH}) FROM ONLY $from);
             LET $moved = (UPDATE $batch SET organization_ids = array::distinct(
                 array::map(organization_ids, |$id| IF $id = $from THEN $to ELSE $id END)
             ) RETURN VALUE id);
             IF array::len($moved) < array::len($batch) {{
                 THROW 'the session may not write every record of the organization';
             }};
             RETURN $moved;"
        ))
        .bind(("from", from))
        .bind(("to", to))
    })
    .await
}

#[cfg(test)]
mod tests {
    use resysto_model::MemberId;
    use resysto_surrealdb::{Actor, Db, Grant, Via};
    use surrealdb::Surreal;
    use surrealdb::engine::any::Any;
    use surrealdb::types::{RecordId, Uuid, Value};

    use super::super::model::Industry;
    use super::*;

    fn in_ui(member: RecordId) -> Actor {
        Actor {
            member,
            via: Via::Ui,
            session: None,
            request: None,
            ip: None,
            user_agent: None,
        }
    }

    // a tenant plus a table that tags its rows like every bounded context does
    async fn tenant() -> Surreal<Any> {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, "t_acme")
            .await
            .unwrap();
        let session = db.system_session("t_acme").await.unwrap();
        session
            .query(
                "DEFINE TABLE item SCHEMAFULL PERMISSIONS FULL;
                 DEFINE FIELD organization_ids ON item TYPE array<record<organization>>
                     REFERENCE ON DELETE REJECT ASSERT array::len($value) > 0;",
            )
            .await
            .unwrap()
            .check()
            .unwrap();
        session
    }

    async fn organization(db: &Surreal<Any>) -> OrganizationId {
        let id = OrganizationId::new();
        let acme = Organization {
            name: "acme".into(),
            phone: Some("06 123".into()),
            ..Default::default()
        };
        insert(db, id, acme).await.unwrap().unwrap();
        id
    }

    async fn tag(db: &Surreal<Any>, organizations: &[OrganizationId]) -> RecordId {
        db.query("CREATE ONLY item SET organization_ids = $ids RETURN VALUE id")
            .bind(("ids", organizations.to_vec()))
            .await
            .unwrap()
            .check()
            .unwrap()
            .take::<Option<RecordId>>(0)
            .unwrap()
            .unwrap()
    }

    async fn tags_of(db: &Surreal<Any>, item: &RecordId) -> Vec<OrganizationId> {
        let tags: Vec<Vec<OrganizationId>> = db
            .query("SELECT VALUE organization_ids FROM $item")
            .bind(("item", item.clone()))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        tags.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn a_write_records_who_made_it_and_the_change() {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, "t_acme")
            .await
            .unwrap();
        let member = MemberId::new();
        let everything = Grant {
            organizations: None,
            write: true,
        };
        let session = db
            .record_session("t_acme", &in_ui(member.into()), &everything)
            .await
            .unwrap();

        let id = organization(&session).await;

        let stored = find(&session, id).await.unwrap().unwrap();
        assert_eq!(
            (stored.created_by, stored.updated_by),
            (Some(member), Some(member))
        );
        let by: Vec<Option<MemberId>> = db
            .system_session("t_acme")
            .await
            .unwrap()
            .query("SELECT VALUE by FROM audit_change WHERE record = $id AND event = 'CREATE'")
            .bind(("id", id))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(by, [Some(member)]);
    }

    #[tokio::test]
    async fn an_update_writes_the_organization_and_removes_the_fields_set_to_none() {
        let db = tenant().await;
        let id = organization(&db).await;
        let mut acme = find(&db, id).await.unwrap().unwrap().data;
        acme.phone = None;
        // a variant of two words, in the form the schema's literal union lists
        acme.industry = Some(Industry::RealEstate);

        update(&db, id, acme).await.unwrap();

        let stored = find(&db, id).await.unwrap().unwrap().data;
        assert_eq!(stored.phone, None);
        assert_eq!(stored.industry, Some(Industry::RealEstate));
    }

    #[tokio::test]
    async fn reassign_replaces_the_organization_in_every_record_once() {
        let db = tenant().await;
        let x = organization(&db).await;
        let y = organization(&db).await;
        let z = organization(&db).await;
        let only_x = tag(&db, &[x]).await;
        let x_and_z = tag(&db, &[x, z]).await;
        let x_and_y = tag(&db, &[x, y]).await;
        let only_z = tag(&db, &[z]).await;
        // more than one batch
        let bulk = u64::from(REASSIGN_BATCH) * 2 + 7;
        db.query("INSERT INTO item array::map(array::range(0, $n), |$i| { organization_ids: [$x] }) RETURN NONE")
            .bind(("n", bulk))
            .bind(("x", x))
            .await
            .unwrap()
            .check()
            .unwrap();

        let moved = reassign(&db, x, y).await.unwrap();

        assert_eq!(moved, bulk + 3);
        assert_eq!(tags_of(&db, &only_x).await, vec![y]);
        assert_eq!(tags_of(&db, &x_and_z).await, vec![y, z]);
        assert_eq!(tags_of(&db, &x_and_y).await, vec![y]);
        assert_eq!(tags_of(&db, &only_z).await, vec![z]);
        assert_eq!(reassign(&db, x, y).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_page_reads_the_id_index_backward_under_the_select_permission() {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, "t_acme")
            .await
            .unwrap();
        let scoped = Grant {
            organizations: Some(Vec::new()),
            write: false,
        };
        let user = RecordId::new("member", Uuid::new_v7());
        let session = db
            .record_session("t_acme", &in_ui(user), &scoped)
            .await
            .unwrap();

        let plan: Value = session
            .query(format!("{LIST} EXPLAIN"))
            .bind(("before", Uuid::new_v7()))
            .bind(("limit", 21))
            .await
            .unwrap()
            .check()
            .unwrap()
            .take(0)
            .unwrap();

        let plan = format!("{plan:?}");
        assert!(
            plan.contains("IndexScan") && plan.contains("Backward"),
            "organization lists without reading its id index backward: {plan}"
        );
    }
}
