use std::collections::BTreeSet;

use resysto_model::{Email, OrganizationId, PersonId};
use resysto_surrealdb::{Page, PageRequest, Tx, transaction};
use serde::{Deserialize, Deserializer, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Datetime, SurrealValue};
use validator::{Validate, ValidationError};

use super::store::{self, Content};

#[derive(Debug, Clone, PartialEq, Serialize, SurrealValue)]
pub struct PersonRecord {
    pub id: PersonId,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<Email>,
    pub phone: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub organization_ids: Vec<OrganizationId>,
    pub created_at: Datetime,
    pub updated_at: Datetime,
}

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
    #[error("outside what the session may write")]
    Forbidden,
    #[error(transparent)]
    Db(#[from] resysto_surrealdb::Error),
}

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
        .ok_or(PersonError::Forbidden))
}

pub async fn delete(db: &Surreal<Any>, id: PersonId) -> Result<(), PersonError> {
    if !store::exists(db, id).await? {
        return Err(PersonError::NotFound);
    }
    if store::delete(db, id).await? {
        Ok(())
    } else {
        Err(PersonError::Forbidden)
    }
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
    use serde_json::json;

    use super::*;

    #[test]
    fn a_name_has_from_1_to_100_characters_once_trimmed() {
        let longest = format!("  {}  ", "à".repeat(100));
        let too_long = "à".repeat(101);
        for name in ["Ada", longest.as_str()] {
            assert!(person_name(name).is_ok(), "refused {name:?}");
        }
        for name in ["", "   ", too_long.as_str()] {
            assert!(person_name(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn a_patch_keeps_absent_fields_clears_nulls_and_sets_values() {
        let acme = OrganizationId::new();
        let roma = OrganizationId::new();
        let mut content = Content {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: Some("ada@acme.it".parse().unwrap()),
            phone: Some("06 123".into()),
            job_title: None,
            department: Some("IT".into()),
            organization_ids: vec![acme],
        };
        let patch: PersonPatch = serde_json::from_value(json!({
            "last_name": " Byron ",
            "phone": null,
            "job_title": "CISO",
            "organization_ids": [roma, acme, roma],
        }))
        .unwrap();

        patch.apply(&mut content);

        assert_eq!(content.first_name, "Ada");
        assert_eq!(content.last_name, "Byron");
        assert_eq!(
            content.email.as_ref().map(Email::as_str),
            Some("ada@acme.it")
        );
        assert_eq!(content.phone, None);
        assert_eq!(content.job_title.as_deref(), Some("CISO"));
        assert_eq!(content.department.as_deref(), Some("IT"));
        assert_eq!(content.organization_ids, vec![roma, acme]);
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
