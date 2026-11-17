//! The SurrealQL of organizations, one function per query

use std::collections::BTreeMap;

use resysto_model::{Country, Email, OrganizationId};
use resysto_surrealdb::{Error, Executor, Page, PageRequest, page, write_in_batches};
use surrealdb::types::SurrealValue;

use super::model::{Industry, OrganizationRecord};

/// Records a reassign rewrites per transaction
pub(super) const REASSIGN_BATCH: u32 = 500;

/// The fields a write sets; the database keeps the id and the timestamps
#[derive(SurrealValue)]
pub(super) struct Content {
    pub(super) name: String,
    pub(super) parent: Option<OrganizationId>,
    pub(super) path: Vec<OrganizationId>,
    pub(super) active: bool,
    pub(super) org_type: Option<String>,
    pub(super) description: Option<String>,
    pub(super) notes: Option<String>,
    pub(super) email: Option<Email>,
    pub(super) phone: Option<String>,
    pub(super) website: Option<String>,
    pub(super) address: Option<String>,
    pub(super) city: Option<String>,
    pub(super) postal_code: Option<String>,
    pub(super) country: Option<Country>,
    pub(super) vat_number: Option<String>,
    pub(super) industry: Option<Industry>,
    pub(super) annual_income: Option<i64>,
    pub(super) employee_count: Option<i64>,
}

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

/// One page of the organizations, in creation order
pub(super) async fn list(
    db: &impl Executor,
    request: PageRequest,
) -> Result<Page<OrganizationRecord>, Error> {
    let query = db.query("SELECT * FROM type::record('organization', $range) LIMIT $limit");
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
    content: Content,
) -> Result<Option<OrganizationRecord>, Error> {
    let created: Option<OrganizationRecord> = db
        .query("CREATE ONLY $id CONTENT $content")
        .bind(("id", id))
        .bind(("content", content))
        .await?
        .check()?
        .take(0)?;
    Ok(created)
}

/// Replaces every field the content carries; a `None` removes the field
pub(super) async fn merge(
    db: &impl Executor,
    id: OrganizationId,
    content: Content,
) -> Result<Option<OrganizationRecord>, Error> {
    let updated: Option<OrganizationRecord> = db
        .query("UPDATE ONLY $id MERGE $content")
        .bind(("id", id))
        .bind(("content", content))
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

impl From<OrganizationRecord> for Content {
    fn from(record: OrganizationRecord) -> Self {
        Self {
            name: record.name,
            parent: record.parent,
            path: record.path,
            active: record.active,
            org_type: record.org_type,
            description: record.description,
            notes: record.notes,
            email: record.email,
            phone: record.phone,
            website: record.website,
            address: record.address,
            city: record.city,
            postal_code: record.postal_code,
            country: record.country,
            vat_number: record.vat_number,
            industry: record.industry,
            annual_income: record.annual_income,
            employee_count: record.employee_count,
        }
    }
}
