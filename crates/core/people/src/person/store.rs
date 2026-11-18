use resysto_model::{OrganizationId, PersonId};
use resysto_surrealdb::{Error, Executor, Page, PageRequest, page};

use super::model::{Person, PersonRecord};

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
    person: Person,
) -> Result<Option<PersonRecord>, Error> {
    let created: Option<PersonRecord> = db
        .query("CREATE ONLY $id CONTENT $person")
        .bind(("id", id))
        .bind(("person", person))
        .await?
        .check()?
        .take(0)?;
    Ok(created)
}

/// Writes every field of `person`, a `None` removes the field; the database keeps the rest
pub(super) async fn update(
    db: &impl Executor,
    id: PersonId,
    person: Person,
) -> Result<Option<PersonRecord>, Error> {
    let updated: Option<PersonRecord> = db
        .query("UPDATE ONLY $id MERGE $person")
        .bind(("id", id))
        .bind(("person", person))
        .await?
        .check()?
        .take(0)?;
    Ok(updated)
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

    fn ada(organization: OrganizationId) -> Person {
        Person {
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
    async fn an_update_removes_the_fields_set_to_none_and_raises_the_version() {
        let db = tenant().await;
        let acme = organization(&db).await;
        let id = PersonId::new();
        let created = insert(&db, id, ada(acme)).await.unwrap().unwrap();

        let cleared = Person {
            phone: None,
            ..ada(acme)
        };
        update(&db, id, cleared).await.unwrap();

        let stored = find(&db, id).await.unwrap().unwrap();
        assert_eq!(stored.data.phone, None);
        assert_eq!(stored.version, created.version + 1);
        assert_eq!(stored.created_at, created.created_at);
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
