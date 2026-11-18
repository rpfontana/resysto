//! Organizations: the types, the rules of the tree and the operations that apply them

use std::collections::{BTreeMap, BTreeSet};

use resysto_model::{Country, Email, OrganizationId};
use resysto_surrealdb::{Page, PageRequest, Tx, transaction};
use serde::{Deserialize, Deserializer, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Datetime, SurrealValue};
use validator::{Validate, ValidationError};

use super::store::{self, Content};

/// How many levels an organization can sit below its root
pub const MAX_DEPTH: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, SurrealValue)]
#[serde(rename_all = "snake_case")]
// untagged writes each variant as its name, the form the schema's literal union expects
#[surreal(untagged, rename_all = "snake_case")]
pub enum Industry {
    Technology,
    Finance,
    Healthcare,
    Manufacturing,
    Retail,
    Education,
    Government,
    Energy,
    Transportation,
    Telecommunications,
    RealEstate,
    Agriculture,
    Entertainment,
    Hospitality,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, SurrealValue)]
pub struct OrganizationRecord {
    pub id: OrganizationId,
    pub name: String,
    pub parent: Option<OrganizationId>,
    /// The ancestors from the root down to the parent
    pub path: Vec<OrganizationId>,
    pub active: bool,
    pub org_type: Option<String>,
    pub description: Option<String>,
    pub notes: Option<String>,
    pub email: Option<Email>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<Country>,
    pub vat_number: Option<String>,
    pub industry: Option<Industry>,
    pub annual_income: Option<i64>,
    pub employee_count: Option<i64>,
    pub created_at: Datetime,
    pub updated_at: Datetime,
}

/// The body of a create
#[derive(Debug, Clone, Default, Deserialize, Validate)]
pub struct NewOrganization {
    #[validate(custom(function = "organization_name"))]
    pub name: String,
    pub parent: Option<OrganizationId>,
    /// `true` when absent
    pub active: Option<bool>,
    #[validate(length(max = 100, message = "at most 100 characters"))]
    pub org_type: Option<String>,
    #[validate(length(max = 2000, message = "at most 2000 characters"))]
    pub description: Option<String>,
    #[validate(length(max = 10000, message = "at most 10000 characters"))]
    pub notes: Option<String>,
    pub email: Option<Email>,
    #[validate(length(max = 50, message = "at most 50 characters"))]
    pub phone: Option<String>,
    #[validate(length(max = 500, message = "at most 500 characters"))]
    pub website: Option<String>,
    #[validate(length(max = 500, message = "at most 500 characters"))]
    pub address: Option<String>,
    #[validate(length(max = 200, message = "at most 200 characters"))]
    pub city: Option<String>,
    #[validate(length(max = 20, message = "at most 20 characters"))]
    pub postal_code: Option<String>,
    pub country: Option<Country>,
    #[validate(length(max = 50, message = "at most 50 characters"))]
    pub vat_number: Option<String>,
    pub industry: Option<Industry>,
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub annual_income: Option<i64>,
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub employee_count: Option<i64>,
}

/// The body of a JSON Merge Patch: an absent field stays, `null` clears it
#[derive(Debug, Clone, Default, Deserialize, Validate)]
pub struct OrganizationPatch {
    #[serde(default, deserialize_with = "present")]
    #[validate(custom(function = "organization_name"))]
    pub name: Option<String>,
    /// `null` moves the organization and its branch to the root
    #[serde(default, deserialize_with = "nullable")]
    pub parent: Option<Option<OrganizationId>>,
    #[serde(default, deserialize_with = "present")]
    pub active: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 100, message = "at most 100 characters"))]
    pub org_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 2000, message = "at most 2000 characters"))]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 10000, message = "at most 10000 characters"))]
    pub notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub email: Option<Option<Email>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 50, message = "at most 50 characters"))]
    pub phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 500, message = "at most 500 characters"))]
    pub website: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 500, message = "at most 500 characters"))]
    pub address: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 200, message = "at most 200 characters"))]
    pub city: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 20, message = "at most 20 characters"))]
    pub postal_code: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub country: Option<Option<Country>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(length(max = 50, message = "at most 50 characters"))]
    pub vat_number: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub industry: Option<Option<Industry>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub annual_income: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub employee_count: Option<Option<i64>>,
}

#[derive(Debug, thiserror::Error)]
pub enum OrganizationError {
    #[error("organization not found")]
    NotFound,
    #[error("parent organization not found")]
    ParentNotFound,
    #[error("an organization cannot be its own parent")]
    OwnParent,
    #[error("an organization cannot move under one of its descendants")]
    Cycle,
    #[error("organizations nest at most {MAX_DEPTH} levels below their root")]
    TooDeep,
    #[error("the organization has child organizations")]
    HasChildren,
    #[error("the organization is still used by {}", uses_text(.0))]
    InUse(BTreeMap<String, i64>),
    #[error("an organization cannot be reassigned to itself")]
    SameTarget,
    #[error("target organization not found")]
    TargetNotFound,
    #[error("outside what the session may write")]
    Forbidden,
    #[error(transparent)]
    Db(#[from] resysto_surrealdb::Error),
}

/// Creates an organization, at the root or under `parent`
pub async fn create(
    db: &Surreal<Any>,
    input: NewOrganization,
) -> Result<OrganizationRecord, OrganizationError> {
    let path = match input.parent {
        None => Vec::new(),
        Some(parent) => {
            let mut path = store::path(db, parent)
                .await?
                .ok_or(OrganizationError::ParentNotFound)?;
            path.push(parent);
            if path.len() > MAX_DEPTH {
                return Err(OrganizationError::TooDeep);
            }
            path
        }
    };
    let content = Content {
        name: input.name.trim().to_owned(),
        parent: input.parent,
        path,
        active: input.active.unwrap_or(true),
        org_type: input.org_type,
        description: input.description,
        notes: input.notes,
        email: input.email,
        phone: input.phone,
        website: input.website,
        address: input.address,
        city: input.city,
        postal_code: input.postal_code,
        country: input.country,
        vat_number: input.vat_number,
        industry: input.industry,
        annual_income: input.annual_income,
        employee_count: input.employee_count,
    };
    store::insert(db, OrganizationId::new(), content)
        .await?
        .ok_or(OrganizationError::Forbidden)
}

pub async fn get(
    db: &Surreal<Any>,
    id: OrganizationId,
) -> Result<OrganizationRecord, OrganizationError> {
    store::find(db, id)
        .await?
        .ok_or(OrganizationError::NotFound)
}

/// One page of the organizations, in creation order
pub async fn list(
    db: &Surreal<Any>,
    request: PageRequest,
) -> Result<Page<OrganizationRecord>, OrganizationError> {
    Ok(store::list(db, request).await?)
}

/// Applies `patch`; a new parent moves the whole branch below the organization with it
pub async fn update(
    db: &Surreal<Any>,
    id: OrganizationId,
    patch: OrganizationPatch,
) -> Result<OrganizationRecord, OrganizationError> {
    transaction(db, |tx| {
        let patch = patch.clone();
        async move { update_in(&tx, id, patch).await }
    })
    .await?
}

// a refusal comes back as Ok(Err(..)): the checks run before any write, so committing is harmless
async fn update_in(
    tx: &Tx,
    id: OrganizationId,
    patch: OrganizationPatch,
) -> Result<Result<OrganizationRecord, OrganizationError>, resysto_surrealdb::Error> {
    let Some(current) = store::find(tx, id).await? else {
        return Ok(Err(OrganizationError::NotFound));
    };
    let old_path = current.path.clone();
    let mut content = Content::from(current);

    if let Some(parent) = patch.parent
        && parent != content.parent
    {
        let path = match parent {
            None => Vec::new(),
            Some(parent) if parent == id => return Ok(Err(OrganizationError::OwnParent)),
            Some(parent) => {
                let Some(mut path) = store::path(tx, parent).await? else {
                    return Ok(Err(OrganizationError::ParentNotFound));
                };
                if path.contains(&id) {
                    return Ok(Err(OrganizationError::Cycle));
                }
                path.push(parent);
                path
            }
        };
        // how many levels the branch has below the organization
        let height = store::depths_below(tx, id)
            .await?
            .into_iter()
            .filter_map(|depth| usize::try_from(depth).ok())
            .max()
            .map_or(0, |deepest| deepest.saturating_sub(old_path.len()));
        if path.len() + height > MAX_DEPTH {
            return Ok(Err(OrganizationError::TooDeep));
        }
        store::rehome_branch(tx, id, old_path, path.clone()).await?;
        content.parent = parent;
        content.path = path;
    }

    patch.apply(&mut content);
    Ok(store::merge(tx, id, content)
        .await?
        .ok_or(OrganizationError::Forbidden))
}

/// Moves every record of `from` to `to` and returns how many moved
pub async fn reassign(
    db: &Surreal<Any>,
    from: OrganizationId,
    to: OrganizationId,
) -> Result<u64, OrganizationError> {
    if from == to {
        return Err(OrganizationError::SameTarget);
    }
    if !store::exists(db, from).await? {
        return Err(OrganizationError::NotFound);
    }
    if !store::exists(db, to).await? {
        return Err(OrganizationError::TargetNotFound);
    }
    Ok(store::reassign(db, from, to).await?)
}

/// The organizations and every organization below them; an id with no organization stays in
pub async fn with_branches(
    db: &Surreal<Any>,
    organizations: Vec<OrganizationId>,
) -> Result<Vec<OrganizationId>, OrganizationError> {
    let below = store::below(db, &organizations).await?;
    let mut seen = BTreeSet::new();
    Ok(organizations
        .into_iter()
        .chain(below)
        .filter(|id| seen.insert(*id))
        .collect())
}

/// Deletes an organization that has no children and no records
pub async fn delete(db: &Surreal<Any>, id: OrganizationId) -> Result<(), OrganizationError> {
    if !store::exists(db, id).await? {
        return Err(OrganizationError::NotFound);
    }
    if store::has_children(db, id).await? {
        return Err(OrganizationError::HasChildren);
    }
    let uses = store::uses(db, id).await?;
    if !uses.is_empty() {
        return Err(OrganizationError::InUse(uses));
    }
    match store::delete(db, id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(OrganizationError::Forbidden),
        // a record tagged since the check makes the database refuse the delete
        Err(error) => {
            let uses = store::uses(db, id).await?;
            if uses.is_empty() {
                Err(error.into())
            } else {
                Err(OrganizationError::InUse(uses))
            }
        }
    }
}

impl OrganizationPatch {
    // every field but the parent, which moves the branch and is handled before
    fn apply(self, content: &mut Content) {
        if let Some(name) = self.name {
            content.name = name.trim().to_owned();
        }
        if let Some(active) = self.active {
            content.active = active;
        }
        if let Some(org_type) = self.org_type {
            content.org_type = org_type;
        }
        if let Some(description) = self.description {
            content.description = description;
        }
        if let Some(notes) = self.notes {
            content.notes = notes;
        }
        if let Some(email) = self.email {
            content.email = email;
        }
        if let Some(phone) = self.phone {
            content.phone = phone;
        }
        if let Some(website) = self.website {
            content.website = website;
        }
        if let Some(address) = self.address {
            content.address = address;
        }
        if let Some(city) = self.city {
            content.city = city;
        }
        if let Some(postal_code) = self.postal_code {
            content.postal_code = postal_code;
        }
        if let Some(country) = self.country {
            content.country = country;
        }
        if let Some(vat_number) = self.vat_number {
            content.vat_number = vat_number;
        }
        if let Some(industry) = self.industry {
            content.industry = industry;
        }
        if let Some(annual_income) = self.annual_income {
            content.annual_income = annual_income;
        }
        if let Some(employee_count) = self.employee_count {
            content.employee_count = employee_count;
        }
    }
}

fn uses_text(uses: &BTreeMap<String, i64>) -> String {
    let parts: Vec<String> = uses
        .iter()
        .map(|(table, count)| format!("{count} {table}"))
        .collect();
    parts.join(", ")
}

fn organization_name(value: &str) -> Result<(), ValidationError> {
    let length = value.trim().chars().count();
    if (2..=200).contains(&length) {
        Ok(())
    } else {
        Err(ValidationError::new("name").with_message("from 2 to 200 characters".into()))
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
    fn a_name_has_from_2_to_200_characters_once_trimmed() {
        let longest = format!("  {}  ", "à".repeat(200));
        let too_long = "à".repeat(201);
        for name in ["Ab", longest.as_str()] {
            assert!(organization_name(name).is_ok(), "refused {name:?}");
        }
        for name in ["", " A ", too_long.as_str()] {
            assert!(organization_name(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn a_patch_keeps_absent_fields_clears_nulls_and_sets_values() {
        let mut content = Content {
            name: "acme".into(),
            email: Some("info@acme.it".parse().unwrap()),
            phone: Some("06 123".into()),
            ..Default::default()
        };
        let patch: OrganizationPatch = serde_json::from_value(json!({
            "name": " Acme Spa ",
            "phone": null,
            "city": "Roma",
            "industry": "real_estate",
        }))
        .unwrap();

        patch.apply(&mut content);

        assert_eq!(content.name, "Acme Spa");
        assert_eq!(
            content.email.as_ref().map(Email::as_str),
            Some("info@acme.it")
        );
        assert_eq!(content.phone, None);
        assert_eq!(content.city.as_deref(), Some("Roma"));
        assert_eq!(content.industry, Some(Industry::RealEstate));
    }

    #[test]
    fn a_patch_cannot_clear_the_name_or_the_active_flag() {
        for body in [r#"{ "name": null }"#, r#"{ "active": null }"#] {
            let parsed = serde_json::from_str::<OrganizationPatch>(body);
            assert!(parsed.is_err(), "accepted {body}");
        }
    }
}
