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

const LIST: &str =
    "SELECT * FROM person WHERE id < type::record('person', $before) ORDER BY id DESC LIMIT $limit";

pub(super) async fn list(
    db: &impl Executor,
    request: PageRequest,
) -> Result<Page<PersonRecord>, Error> {
    let query = db.query(LIST);
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
    use std::net::IpAddr;

    use resysto_model::MemberId;
    use resysto_surrealdb::{Actor, Db, Grant, Via};
    use surrealdb::Surreal;
    use surrealdb::engine::any::Any;
    use surrealdb::types::{RecordId, SurrealValue, Uuid, Value};

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

    #[derive(Debug, PartialEq, SurrealValue)]
    struct Change {
        event: String,
        by: Option<MemberId>,
        via: Option<String>,
        request: Option<String>,
        ip: Option<String>,
        user_agent: Option<String>,
        phone_before: Option<String>,
        phone_after: Option<String>,
    }

    #[tokio::test]
    async fn every_write_records_who_made_it_and_what_changed() {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, "t_acme")
            .await
            .unwrap();
        let system = db.system_session("t_acme").await.unwrap();
        let acme = organization(&system).await;
        let (ada_member, bob_member) = (MemberId::new(), MemberId::new());
        let everything = Grant {
            organizations: None,
            write: true,
        };
        let from_ada = Actor {
            request: Some("r-1".into()),
            ip: Some(IpAddr::from([10, 0, 0, 7])),
            user_agent: Some("Firefox".into()),
            ..in_ui(ada_member.into())
        };
        let as_ada = db
            .record_session("t_acme", &from_ada, &everything)
            .await
            .unwrap();
        let as_bob = db
            .record_session("t_acme", &in_ui(bob_member.into()), &everything)
            .await
            .unwrap();
        let id = PersonId::new();

        insert(&as_ada, id, ada(acme)).await.unwrap();
        let cleared = Person {
            phone: None,
            ..ada(acme)
        };
        let updated = update(&as_bob, id, cleared).await.unwrap().unwrap();
        assert_eq!(updated.created_by, Some(ada_member));
        assert_eq!(updated.updated_by, Some(bob_member));
        delete(&as_bob, id).await.unwrap();

        let changes: Vec<Change> = system
            .query(
                "SELECT event, by, via, request, ip, user_agent, before.phone AS phone_before,
                     after.phone AS phone_after
                 FROM audit_change WHERE record = $id ORDER BY id",
            )
            .bind(("id", id))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let by_bob = |event: &str, phone_before: Option<String>| Change {
            event: event.into(),
            by: Some(bob_member),
            via: Some("ui".into()),
            request: None,
            ip: None,
            user_agent: None,
            phone_before,
            phone_after: None,
        };
        let expected = [
            Change {
                event: "CREATE".into(),
                by: Some(ada_member),
                via: Some("ui".into()),
                request: Some("r-1".into()),
                ip: Some("10.0.0.7".into()),
                user_agent: Some("Firefox".into()),
                phone_before: None,
                phone_after: Some("06 123".into()),
            },
            by_bob("UPDATE", Some("06 123".into())),
            by_bob("DELETE", None),
        ];
        assert_eq!(changes, expected);
        let seen: Vec<Value> = as_ada
            .query("SELECT * FROM audit_change")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(seen.is_empty(), "a record session read {seen:?}");
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
            "person lists without reading its id index backward: {plan}"
        );
    }
}
