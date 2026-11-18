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

#[cfg(test)]
mod tests {
    use resysto_surrealdb::Db;
    use surrealdb::Surreal;
    use surrealdb::engine::any::Any;

    use super::*;

    async fn tenant() -> Surreal<Any> {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, "t_acme")
            .await
            .unwrap();
        db.system_session("t_acme").await.unwrap()
    }

    async fn organization(db: &Surreal<Any>) -> OrganizationId {
        let id = OrganizationId::new();
        db.query("CREATE $id SET name = 'acme', path = []")
            .bind(("id", id))
            .await
            .unwrap()
            .check()
            .unwrap();
        id
    }

    fn ada(organization: OrganizationId) -> Content {
        Content {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: None,
            phone: Some("06 123".into()),
            job_title: None,
            department: None,
            organization_ids: vec![organization],
        }
    }

    #[tokio::test]
    async fn a_merge_removes_the_fields_set_to_none() {
        let db = tenant().await;
        let acme = organization(&db).await;
        let id = PersonId::new();
        insert(&db, id, ada(acme)).await.unwrap();

        let cleared = Content {
            phone: None,
            ..ada(acme)
        };
        merge(&db, id, cleared).await.unwrap();

        let stored = find(&db, id).await.unwrap().unwrap();
        assert_eq!(stored.phone, None);
    }

    #[tokio::test]
    async fn an_organization_with_people_cannot_be_deleted() {
        let db = tenant().await;
        let acme = organization(&db).await;
        insert(&db, PersonId::new(), ada(acme)).await.unwrap();

        let deleted = db
            .query("DELETE $id")
            .bind(("id", acme))
            .await
            .unwrap()
            .check();

        assert!(deleted.is_err(), "deleted an organization that has people");
    }
}
