use resysto_model::{Email, OrganizationId, PersonId};
use resysto_surrealdb::{Error, Executor, Page, PageRequest, page};
use surrealdb::types::SurrealValue;

use super::model::PersonRecord;

/// The fields a write sets; the database keeps the id and the timestamps
#[derive(SurrealValue)]
pub(super) struct Content {
    pub(super) first_name: String,
    pub(super) last_name: String,
    pub(super) email: Option<Email>,
    pub(super) phone: Option<String>,
    pub(super) job_title: Option<String>,
    pub(super) department: Option<String>,
    pub(super) organization_ids: Vec<OrganizationId>,
}

pub(super) async fn find(db: &impl Executor, id: PersonId) -> Result<Option<PersonRecord>, Error> {
    let found: Option<PersonRecord> = db
        .query("SELECT * FROM ONLY $id")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(found)
}

pub(super) async fn list(
    db: &impl Executor,
    request: PageRequest,
) -> Result<Page<PersonRecord>, Error> {
    let query = db.query("SELECT * FROM type::record('person', $range) LIMIT $limit");
    page(query, request).await
}

pub(super) async fn missing_organizations(
    db: &impl Executor,
    ids: &[OrganizationId],
) -> Result<Vec<OrganizationId>, Error> {
    let missing: Vec<OrganizationId> = db
        .query("RETURN array::filter($ids, |$id| !record::exists($id))")
        .bind(("ids", ids.to_vec()))
        .await?
        .check()?
        .take(0)?;
    Ok(missing)
}

pub(super) async fn insert(
    db: &impl Executor,
    id: PersonId,
    content: Content,
) -> Result<Option<PersonRecord>, Error> {
    let created: Option<PersonRecord> = db
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
    id: PersonId,
    content: Content,
) -> Result<Option<PersonRecord>, Error> {
    let updated: Option<PersonRecord> = db
        .query("UPDATE ONLY $id MERGE $content")
        .bind(("id", id))
        .bind(("content", content))
        .await?
        .check()?
        .take(0)?;
    Ok(updated)
}

pub(super) async fn exists(db: &impl Executor, id: PersonId) -> Result<bool, Error> {
    let exists: Option<bool> = db
        .query("RETURN record::exists($id)")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(exists == Some(true))
}

/// Whether the person went
pub(super) async fn delete(db: &impl Executor, id: PersonId) -> Result<bool, Error> {
    let deleted: Option<PersonId> = db
        .query("DELETE ONLY $id RETURN VALUE $before.id")
        .bind(("id", id))
        .await?
        .check()?
        .take(0)?;
    Ok(deleted.is_some())
}

impl From<PersonRecord> for Content {
    fn from(record: PersonRecord) -> Self {
        Self {
            first_name: record.first_name,
            last_name: record.last_name,
            email: record.email,
            phone: record.phone,
            job_title: record.job_title,
            department: record.department,
            organization_ids: record.organization_ids,
        }
    }
}
