//! People: the types, the rules and the operations that apply them

use std::collections::BTreeSet;

use resysto_model::{Email, OrganizationId, PersonId};
use resysto_surrealdb::{Page, PageRequest, Tx, transaction};
use serde::{Deserialize, Deserializer, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Datetime, SurrealValue};
use validator::{Validate, ValidationError};

use super::store::{self, Content};

/// A person as stored, which the API returns as it is
#[derive(Debug, Clone, PartialEq, Serialize, SurrealValue)]
pub struct PersonRecord {
    pub id: PersonId,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<Email>,
    pub phone: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    /// The organizations the person belongs to, at least one
    pub organization_ids: Vec<OrganizationId>,
    pub created_at: Datetime,
    pub updated_at: Datetime,
}

/// The body of a create
#[derive(Debug, Clone, Default, Deserialize, Validate)]
pub struct NewPerson {
    #[validate(custom(function = "person_name"))]
    pub first_name: String,
    #[validate(custom(function = "person_name"))]
    pub last_name: String,
    pub email: Option<Email>,
    #[validate(length(max = 50, message = "at most 50 characters"))]
    pub phone: Option<String>,
    #[validate(length(max = 200, message = "at most 200 characters"))]
    pub job_title: Option<String>,
    #[validate(length(max = 200, message = "at most 200 characters"))]
    pub department: Option<String>,
    #[validate(length(min = 1, message = "at least one organization"))]
    pub organization_ids: Vec<OrganizationId>,
}

/// The body of a JSON Merge Patch: an absent field stays, `null` clears it
#[derive(Debug, Clone, Default, Deserialize, Validate)]
pub struct PersonPatch {
    #[serde(default, deserialize_with = "present")]
    #[validate(custom(function = "person_name"))]
    pub first_name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    #[validate(custom(function = "person_name"))]
    pub last_name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub email: Option<Option<Email>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 50, message = "at most 50 characters"))]
    pub phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 200, message = "at most 200 characters"))]
    pub job_title: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 200, message = "at most 200 characters"))]
    pub department: Option<Option<String>>,
    /// Replaces the whole list
    #[serde(default, deserialize_with = "present")]
    #[validate(length(min = 1, message = "at least one organization"))]
    pub organization_ids: Option<Vec<OrganizationId>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PersonError {
    #[error("person not found")]
    NotFound,
    #[error("organizations not found: {}", ids_text(.0))]
    OrganizationsNotFound(Vec<OrganizationId>),
    #[error(transparent)]
    Db(#[from] resysto_surrealdb::Error),
}

/// Creates a person in organizations that exist
pub async fn create(db: &Surreal<Any>, input: NewPerson) -> Result<PersonRecord, PersonError> {
    let organization_ids = distinct(input.organization_ids);
    let missing = store::missing_organizations(db, &organization_ids).await?;
    if !missing.is_empty() {
        return Err(PersonError::OrganizationsNotFound(missing));
    }
    let content = Content {
        first_name: input.first_name.trim().to_owned(),
        last_name: input.last_name.trim().to_owned(),
        email: input.email,
        phone: input.phone,
        job_title: input.job_title,
        department: input.department,
        organization_ids,
    };
    store::insert(db, PersonId::new(), content)
        .await?
        .ok_or(PersonError::NotFound)
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

pub async fn update(
    db: &Surreal<Any>,
    id: PersonId,
    patch: PersonPatch,
) -> Result<PersonRecord, PersonError> {
    transaction(db, |tx| {
        let patch = patch.clone();
        async move { update_in(&tx, id, patch).await }
    })
    .await?
}

// a refusal comes back as Ok(Err(..)): the checks run before any write, so committing is harmless
async fn update_in(
    tx: &Tx,
    id: PersonId,
    patch: PersonPatch,
) -> Result<Result<PersonRecord, PersonError>, resysto_surrealdb::Error> {
    let Some(current) = store::find(tx, id).await? else {
        return Ok(Err(PersonError::NotFound));
    };
    if let Some(organization_ids) = &patch.organization_ids {
        let missing = store::missing_organizations(tx, organization_ids).await?;
        if !missing.is_empty() {
            return Ok(Err(PersonError::OrganizationsNotFound(missing)));
        }
    }
    let mut content = Content::from(current);
    patch.apply(&mut content);
    Ok(store::merge(tx, id, content)
        .await?
        .ok_or(PersonError::NotFound))
}

pub async fn delete(db: &Surreal<Any>, id: PersonId) -> Result<(), PersonError> {
    if !store::exists(db, id).await? {
        return Err(PersonError::NotFound);
    }
    Ok(store::delete(db, id).await?)
}

impl PersonPatch {
    fn apply(self, content: &mut Content) {
        if let Some(first_name) = self.first_name {
            content.first_name = first_name.trim().to_owned();
        }
        if let Some(last_name) = self.last_name {
            content.last_name = last_name.trim().to_owned();
        }
        if let Some(email) = self.email {
            content.email = email;
        }
        if let Some(phone) = self.phone {
            content.phone = phone;
        }
        if let Some(job_title) = self.job_title {
            content.job_title = job_title;
        }
        if let Some(department) = self.department {
            content.department = department;
        }
        if let Some(organization_ids) = self.organization_ids {
            content.organization_ids = distinct(organization_ids);
        }
    }
}

// an organization listed twice adds nothing; the first occurrence keeps its place
fn distinct(ids: Vec<OrganizationId>) -> Vec<OrganizationId> {
    let mut seen = BTreeSet::new();
    ids.into_iter().filter(|id| seen.insert(*id)).collect()
}

fn ids_text(ids: &[OrganizationId]) -> String {
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    ids.join(", ")
}

fn person_name(value: &str) -> Result<(), ValidationError> {
    let length = value.trim().chars().count();
    if (1..=100).contains(&length) {
        Ok(())
    } else {
        Err(ValidationError::new("name").with_message("from 1 to 100 characters".into()))
    }
}

// absent is None, null is refused
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

// absent is None, null is Some(None)
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use resysto_surrealdb::Db;
    use secrecy::SecretString;
    use surrealdb::types::Uuid;

    use super::*;

    #[derive(Deserialize)]
    struct Config {
        db: DbConfig,
        auth: AuthConfig,
    }

    #[derive(Deserialize)]
    struct DbConfig {
        endpoint: String,
        namespace: String,
        migrator_user: String,
        migrator_password: SecretString,
    }

    #[derive(Deserialize)]
    struct AuthConfig {
        signing_key: SecretString,
        verifying_key: String,
    }

    // a new tenant with the whole tenant lane
    async fn tenant() -> Surreal<Any> {
        let Config { db, auth } = resysto_config::load().unwrap();
        let server = Db::connect(resysto_surrealdb::Config {
            endpoint: db.endpoint,
            namespace: db.namespace,
            username: db.migrator_user,
            password: db.migrator_password,
            signing_key: auth.signing_key,
            verifying_key: auth.verifying_key,
        })
        .await
        .unwrap();
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        resysto_migrator::provision_tenant(&server, &name)
            .await
            .unwrap();
        server.system_session(&name).await.unwrap()
    }

    // written by hand: a bounded context does not depend on another one
    async fn organization(db: &Surreal<Any>, name: &str) -> OrganizationId {
        db.query("CREATE ONLY organization SET name = $name, path = [] RETURN VALUE id")
            .bind(("name", name.to_owned()))
            .await
            .unwrap()
            .check()
            .unwrap()
            .take::<Option<OrganizationId>>(0)
            .unwrap()
            .unwrap()
    }

    fn ada(organization_ids: Vec<OrganizationId>) -> NewPerson {
        NewPerson {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            organization_ids,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_person_belongs_only_to_organizations_that_exist() {
        let db = tenant().await;
        let acme = organization(&db, "acme").await;
        let ghost = OrganizationId::new();

        let refused = create(&db, ada(vec![acme, ghost])).await;
        let Err(PersonError::OrganizationsNotFound(missing)) = refused else {
            panic!("expected OrganizationsNotFound, got {refused:?}");
        };
        assert_eq!(missing, vec![ghost]);

        let created = create(&db, ada(vec![acme, acme])).await.unwrap();
        assert_eq!(created.organization_ids, vec![acme]);

        let patch = PersonPatch {
            organization_ids: Some(vec![ghost]),
            ..Default::default()
        };
        let refused = update(&db, created.id, patch).await;
        assert!(
            matches!(refused, Err(PersonError::OrganizationsNotFound(_))),
            "{refused:?}"
        );
        let kept = get(&db, created.id).await.unwrap().organization_ids;
        assert_eq!(kept, vec![acme]);
    }

    #[tokio::test]
    async fn an_organization_with_people_cannot_be_deleted() {
        let db = tenant().await;
        let acme = organization(&db, "acme").await;
        create(&db, ada(vec![acme])).await.unwrap();

        let deleted = db
            .query("DELETE $id")
            .bind(("id", acme))
            .await
            .unwrap()
            .check();

        assert!(deleted.is_err(), "deleted an organization that has people");
    }

    #[tokio::test]
    async fn a_patch_keeps_absent_fields_clears_nulls_and_sets_values() {
        let db = tenant().await;
        let acme = organization(&db, "acme").await;
        let input = NewPerson {
            email: Some("ada@acme.it".parse().unwrap()),
            phone: Some("06 123".into()),
            ..ada(vec![acme])
        };
        let created = create(&db, input).await.unwrap();

        let patch: PersonPatch = serde_json::from_str(
            r#"{ "last_name": " Byron ", "phone": null, "job_title": "CISO" }"#,
        )
        .unwrap();
        let updated = update(&db, created.id, patch).await.unwrap();

        assert_eq!(updated.first_name, "Ada");
        assert_eq!(updated.last_name, "Byron");
        assert_eq!(
            updated.email.as_ref().map(Email::as_str),
            Some("ada@acme.it")
        );
        assert_eq!(updated.phone, None);
        assert_eq!(updated.job_title.as_deref(), Some("CISO"));
        assert_eq!(updated.organization_ids, vec![acme]);
        assert_eq!(updated.created_at, created.created_at);
    }

    #[test]
    fn a_patch_cannot_clear_the_names_or_the_organizations() {
        let bodies = [
            r#"{ "first_name": null }"#,
            r#"{ "last_name": null }"#,
            r#"{ "organization_ids": null }"#,
        ];
        for body in bodies {
            let parsed = serde_json::from_str::<PersonPatch>(body);
            assert!(parsed.is_err(), "accepted {body}");
        }
    }
}
