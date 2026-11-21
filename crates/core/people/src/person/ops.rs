use resysto_model::{OrganizationId, PersonId};
use resysto_surrealdb::{Page, PageRequest, Tx, transaction};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use super::model::{NewPerson, Person, PersonPatch, PersonRecord};
use super::store;

#[derive(Debug, thiserror::Error)]
pub enum PersonError {
    #[error("person not found")]
    NotFound,
    #[error("the person changed since it was read: its version is now {0}")]
    Stale(i64),
    #[error("the patch changes no field")]
    NothingToChange,
    #[error("organizations not found: {}", ids_text(.0))]
    OrganizationsNotFound(Vec<OrganizationId>),
    #[error("outside what the session may write")]
    Forbidden,
    #[error(transparent)]
    Db(#[from] resysto_surrealdb::Error),
}

pub async fn create(db: &Surreal<Any>, input: NewPerson) -> Result<PersonRecord, PersonError> {
    let person = Person::from(input);
    let missing = store::missing_organizations(db, &person.organization_ids).await?;
    if !missing.is_empty() {
        return Err(PersonError::OrganizationsNotFound(missing));
    }
    store::insert(db, PersonId::new(), person)
        .await?
        .ok_or(PersonError::Forbidden)
}

pub async fn get(db: &Surreal<Any>, id: PersonId) -> Result<PersonRecord, PersonError> {
    store::find(db, id).await?.ok_or(PersonError::NotFound)
}

/// One page of the people, newest first
pub async fn list(
    db: &Surreal<Any>,
    request: PageRequest,
) -> Result<Page<PersonRecord>, PersonError> {
    Ok(store::list(db, request).await?)
}

pub(super) async fn search(
    db: &Surreal<Any>,
    text: Option<&str>,
    organization: Option<OrganizationId>,
    request: PageRequest,
) -> Result<Page<PersonRecord>, PersonError> {
    let text = text
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);
    Ok(store::search(db, text, organization, request).await?)
}

/// Applies `patch` to the person, if it is still at `version`
pub async fn update(
    db: &Surreal<Any>,
    id: PersonId,
    version: i64,
    patch: PersonPatch,
) -> Result<PersonRecord, PersonError> {
    if patch == PersonPatch::default() {
        return Err(PersonError::NothingToChange);
    }
    transaction(db, |tx| {
        let patch = patch.clone();
        async move { update_in(&tx, id, version, patch).await }
    })
    .await?
}

// a refusal comes back as Ok(Err(..)): the checks run before any write, so committing is harmless
async fn update_in(
    tx: &Tx,
    id: PersonId,
    version: i64,
    patch: PersonPatch,
) -> Result<Result<PersonRecord, PersonError>, resysto_surrealdb::Error> {
    let Some(record) = store::find(tx, id).await? else {
        return Ok(Err(PersonError::NotFound));
    };
    if record.version != version {
        return Ok(Err(PersonError::Stale(record.version)));
    }
    if let Some(organization_ids) = &patch.organization_ids {
        let missing = store::missing_organizations(tx, organization_ids).await?;
        if !missing.is_empty() {
            return Ok(Err(PersonError::OrganizationsNotFound(missing)));
        }
    }
    let mut person = record.data;
    patch.apply(&mut person);
    Ok(store::update(tx, id, person)
        .await?
        .ok_or(PersonError::Forbidden))
}

/// Deletes the person, if it is still at `version`
pub async fn delete(db: &Surreal<Any>, id: PersonId, version: i64) -> Result<(), PersonError> {
    transaction(db, |tx| async move { delete_in(&tx, id, version).await }).await?
}

async fn delete_in(
    tx: &Tx,
    id: PersonId,
    version: i64,
) -> Result<Result<(), PersonError>, resysto_surrealdb::Error> {
    let Some(record) = store::find(tx, id).await? else {
        return Ok(Err(PersonError::NotFound));
    };
    if record.version != version {
        return Ok(Err(PersonError::Stale(record.version)));
    }
    Ok(if store::delete(tx, id).await? {
        Ok(())
    } else {
        Err(PersonError::Forbidden)
    })
}

fn ids_text(ids: &[OrganizationId]) -> String {
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    ids.join(", ")
}

#[cfg(test)]
mod tests {
    use resysto_model::MemberId;
    use resysto_surrealdb::{Actor, Db, Grant, Via};

    use super::*;

    const TENANT: &str = "t_acme";

    async fn tenant() -> Db {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, TENANT)
            .await
            .unwrap();
        db
    }

    // organizations belong to another bounded context, so they are written as rows
    async fn organization(db: &Db) -> OrganizationId {
        let id = OrganizationId::new();
        db.system_session(TENANT)
            .await
            .unwrap()
            .query("CREATE $id SET name = 'acme', path = []")
            .bind(("id", id))
            .await
            .unwrap()
            .check()
            .unwrap();
        id
    }

    async fn session(db: &Db, grant: &Grant) -> Surreal<Any> {
        let actor = Actor {
            member: MemberId::new().into(),
            via: Via::Ui,
            client: None,
            session: None,
            request: None,
            ip: None,
            user_agent: None,
        };
        db.record_session(TENANT, &actor, grant).await.unwrap()
    }

    fn everything() -> Grant {
        Grant {
            organizations: None,
            write: true,
        }
    }

    fn within(organization: OrganizationId, write: bool) -> Grant {
        Grant {
            organizations: Some(vec![organization.into()]),
            write,
        }
    }

    fn ada(organization_ids: &[OrganizationId]) -> NewPerson {
        NewPerson {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            organization_ids: organization_ids.to_vec(),
            ..NewPerson::default()
        }
    }

    fn job_title(title: &str) -> PersonPatch {
        PersonPatch {
            job_title: Some(Some(title.into())),
            ..PersonPatch::default()
        }
    }

    #[tokio::test]
    async fn a_person_belongs_only_to_organizations_that_exist() {
        let db = tenant().await;
        let acme = organization(&db).await;
        let ghost = OrganizationId::new();
        let all = session(&db, &everything()).await;

        let refused = create(&all, ada(&[acme, ghost])).await;
        let Err(PersonError::OrganizationsNotFound(missing)) = &refused else {
            panic!("created with a missing organization: {refused:?}");
        };
        assert_eq!(*missing, [ghost]);

        let person = create(&all, ada(&[acme])).await.unwrap();
        let moved = PersonPatch {
            organization_ids: Some(vec![ghost]),
            ..PersonPatch::default()
        };
        let refused = update(&all, person.id, person.version, moved).await;
        assert!(
            matches!(refused, Err(PersonError::OrganizationsNotFound(_))),
            "{refused:?}"
        );
        let kept = get(&all, person.id).await.unwrap();
        assert_eq!(kept.data.organization_ids, [acme]);
    }

    #[tokio::test]
    async fn a_scoped_member_reads_and_writes_only_the_people_of_its_organizations() {
        let db = tenant().await;
        let milano = organization(&db).await;
        let roma = organization(&db).await;
        let all = session(&db, &everything()).await;
        let in_milano = create(&all, ada(&[milano])).await.unwrap();
        let in_roma = create(&all, ada(&[roma])).await.unwrap();
        let in_both = create(&all, ada(&[milano, roma])).await.unwrap();
        let scoped = session(&db, &within(milano, true)).await;

        let listed = list(&scoped, PageRequest::default()).await.unwrap();
        let seen: Vec<PersonId> = listed.data.iter().map(|person| person.id).collect();
        assert_eq!(seen, [in_both.id, in_milano.id]);

        let outside = get(&scoped, in_roma.id).await;
        assert!(matches!(outside, Err(PersonError::NotFound)), "{outside:?}");
        // an organization outside the scope reads as one that does not exist
        let elsewhere = create(&scoped, ada(&[roma])).await;
        assert!(
            matches!(elsewhere, Err(PersonError::OrganizationsNotFound(_))),
            "{elsewhere:?}"
        );
        let changed = update(&scoped, in_both.id, in_both.version, job_title("CISO")).await;
        assert!(
            matches!(changed, Err(PersonError::Forbidden)),
            "{changed:?}"
        );
        let deleted = delete(&scoped, in_both.id, in_both.version).await;
        assert!(
            matches!(deleted, Err(PersonError::Forbidden)),
            "{deleted:?}"
        );
        let kept = get(&all, in_both.id).await.unwrap();
        assert_eq!(kept.version, in_both.version);

        let reading = session(&db, &within(milano, false)).await;
        let added = create(&reading, ada(&[milano])).await;
        assert!(matches!(added, Err(PersonError::Forbidden)), "{added:?}");
    }

    #[tokio::test]
    async fn a_write_on_a_version_already_changed_is_refused_and_changes_nothing() {
        let db = tenant().await;
        let acme = organization(&db).await;
        let all = session(&db, &everything()).await;
        let read = create(&all, ada(&[acme])).await.unwrap();
        let first = update(&all, read.id, read.version, job_title("CISO"))
            .await
            .unwrap();

        // a second writer that read the person before the first one wrote
        let second = update(&all, read.id, read.version, job_title("CTO")).await;
        assert!(
            matches!(second, Err(PersonError::Stale(now)) if now == first.version),
            "{second:?}"
        );
        let deleted = delete(&all, read.id, read.version).await;
        assert!(matches!(deleted, Err(PersonError::Stale(_))), "{deleted:?}");
        let kept = get(&all, read.id).await.unwrap();
        assert_eq!(kept.data.job_title.as_deref(), Some("CISO"));

        delete(&all, read.id, first.version).await.unwrap();
        let gone = get(&all, read.id).await;
        assert!(matches!(gone, Err(PersonError::NotFound)), "{gone:?}");
    }
}
