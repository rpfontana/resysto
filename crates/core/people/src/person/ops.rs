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

/// One page of the people, in creation order
pub async fn list(
    db: &Surreal<Any>,
    request: PageRequest,
) -> Result<Page<PersonRecord>, PersonError> {
    Ok(store::list(db, request).await?)
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
