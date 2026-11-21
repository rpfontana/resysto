use std::collections::BTreeMap;
use std::str::FromStr;

use resysto_mcp::{Effect, ResponseFormat, Tool, ToolError, limit};
use resysto_model::{Email, MemberId, OrganizationId, PersonId};
use resysto_surrealdb::{Cursor, PageRequest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::Datetime;
use validator::Validate;

use super::model::{NewPerson, PersonPatch, PersonRecord};
use super::ops::{self, PersonError};
use super::store::{self, OrganizationName};

const SEARCH: &str = "Finds the people of the tenant: employees, contacts, the owners of risks and \
controls. Matches part of a first name, last name or email, ignoring case, and can keep only the \
people of one organization. Call it to find who someone is, to list who works somewhere, and to \
get a person's id and version before people_update. Answers the newest first, with a next_cursor \
when there are more.";

const ADD: &str = "Adds a person to the tenant, in one or more organizations. Name each \
organization by the id org_search gives, or by its exact name; a name that fits more than one \
organization is refused with the candidates, so call again with the right id. Search first with \
people_search: the tenant may already know the person.";

const UPDATE: &str = "Changes some fields of one person, by the id people_search gives. Fields \
left out stay as they are; list in clear the ones to empty. Pass the version people_search showed \
as expected_version, so the change is refused if someone else changed the person meanwhile; \
without it, the change applies to whatever the person holds now.";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("people_search", Effect::Read, SEARCH, search),
        Tool::new("people_add", Effect::Write, ADD, add),
        Tool::new("people_update", Effect::Write, UPDATE, update),
    ]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    /// Part of a first name, last name or email; leave it out to list everyone
    text: Option<String>,
    /// Only the people of this organization, by the id org_search gives
    organization_id: Option<String>,
    /// How many people to return: 20 when left out, at most 50
    limit: Option<u32>,
    /// The next_cursor of the previous answer, to read on
    cursor: Option<String>,
    #[serde(default)]
    response_format: ResponseFormat,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Add {
    first_name: String,
    last_name: String,
    email: Option<String>,
    phone: Option<String>,
    job_title: Option<String>,
    department: Option<String>,
    /// The organizations the person belongs to, at least one: ids from org_search or exact names
    organizations: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Update {
    /// The person to change, by the id people_search gives
    person_id: String,
    first_name: Option<String>,
    last_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    job_title: Option<String>,
    department: Option<String>,
    /// Replaces all the person's organizations: ids from org_search or exact names
    organizations: Option<Vec<String>>,
    /// Fields to empty
    #[serde(default)]
    clear: Vec<Clearable>,
    /// The version people_search showed; the change is refused if the person changed since
    expected_version: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Clearable {
    Email,
    Phone,
    JobTitle,
    Department,
}

#[derive(Debug, Serialize)]
struct Found {
    people: Vec<PersonView>,
    next_cursor: Option<Cursor>,
}

#[derive(Debug, Serialize)]
struct PersonView {
    id: PersonId,
    name: String,
    email: Option<Email>,
    job_title: Option<String>,
    organizations: Vec<String>,
    version: i64,
    #[serde(flatten)]
    details: Option<Details>,
}

#[derive(Debug, Serialize)]
struct Details {
    phone: Option<String>,
    department: Option<String>,
    organization_ids: Vec<OrganizationId>,
    created_at: Datetime,
    created_by: Option<MemberId>,
    updated_at: Datetime,
    updated_by: Option<MemberId>,
}

async fn search(db: Surreal<Any>, input: Search) -> Result<Found, ToolError> {
    let organization = input
        .organization_id
        .as_deref()
        .map(|id| {
            OrganizationId::from_str(id).map_err(|_| {
                ToolError::refused(format!(
                    "organization_id {id:?} is not an organization id: take one from org_search"
                ))
            })
        })
        .transpose()?;
    let request = PageRequest {
        after: input.cursor.as_deref().map(cursor).transpose()?,
        limit: Some(limit(input.limit)),
    };
    let page = ops::search(&db, input.text.as_deref(), organization, request)
        .await
        .map_err(refusal)?;
    let names = names_of(&db, &page.data).await?;
    let people = page
        .data
        .into_iter()
        .map(|person| view(person, &names, input.response_format))
        .collect();
    Ok(Found {
        people,
        next_cursor: page.next_cursor,
    })
}

async fn add(db: Surreal<Any>, input: Add) -> Result<PersonView, ToolError> {
    let organization_ids = resolve(&db, &input.organizations).await?;
    let new = NewPerson {
        first_name: input.first_name,
        last_name: input.last_name,
        email: input.email.as_deref().map(email).transpose()?,
        phone: input.phone,
        job_title: input.job_title,
        department: input.department,
        organization_ids,
    };
    new.validate()?;
    let created = ops::create(&db, new).await.map_err(refusal)?;
    let names = names_of(&db, std::slice::from_ref(&created)).await?;
    Ok(view(created, &names, ResponseFormat::Concise))
}

async fn update(db: Surreal<Any>, input: Update) -> Result<PersonView, ToolError> {
    let id = PersonId::from_str(&input.person_id).map_err(|_| {
        ToolError::refused(format!(
            "person_id {:?} is not a person id: take one from people_search",
            input.person_id
        ))
    })?;
    let cleared = |field| input.clear.contains(&field);
    let organization_ids = match &input.organizations {
        Some(organizations) => Some(resolve(&db, organizations).await?),
        None => None,
    };
    let patch = PersonPatch {
        first_name: input.first_name,
        last_name: input.last_name,
        email: match input.email.as_deref() {
            Some(raw) => Some(Some(email(raw)?)),
            None => cleared(Clearable::Email).then_some(None),
        },
        phone: input
            .phone
            .map(Some)
            .or_else(|| cleared(Clearable::Phone).then_some(None)),
        job_title: input
            .job_title
            .map(Some)
            .or_else(|| cleared(Clearable::JobTitle).then_some(None)),
        department: input
            .department
            .map(Some)
            .or_else(|| cleared(Clearable::Department).then_some(None)),
        organization_ids,
    };
    patch.validate()?;
    let version = match input.expected_version {
        Some(version) => version,
        None => ops::get(&db, id).await.map_err(refusal)?.version,
    };
    let updated = ops::update(&db, id, version, patch)
        .await
        .map_err(refusal)?;
    let names = names_of(&db, std::slice::from_ref(&updated)).await?;
    Ok(view(updated, &names, ResponseFormat::Concise))
}

async fn resolve(
    db: &Surreal<Any>,
    organizations: &[String],
) -> Result<Vec<OrganizationId>, ToolError> {
    let mut ids = Vec::with_capacity(organizations.len());
    for entry in organizations {
        if let Ok(id) = OrganizationId::from_str(entry) {
            ids.push(id);
            continue;
        }
        let named = store::organizations_named(db, entry).await?;
        match named.as_slice() {
            [] => {
                return Err(ToolError::refused(format!(
                    "no organization is named {entry:?}: find it with org_search and pass its id"
                )));
            }
            [only] => ids.push(only.id),
            several => {
                let candidates: Vec<String> = several
                    .iter()
                    .map(|found| format!("{} ({})", path_of(found), found.id))
                    .collect();
                return Err(ToolError::refused(format!(
                    "{entry:?} names {} organizations: {}; pass the id of the one you mean",
                    several.len(),
                    candidates.join(", ")
                )));
            }
        }
    }
    Ok(ids)
}

async fn names_of(
    db: &Surreal<Any>,
    people: &[PersonRecord],
) -> Result<BTreeMap<OrganizationId, String>, ToolError> {
    let ids: Vec<OrganizationId> = people
        .iter()
        .flat_map(|person| person.data.organization_ids.iter().copied())
        .collect();
    let names = store::organization_names(db, ids).await?;
    Ok(names
        .iter()
        .map(|found| (found.id, path_of(found)))
        .collect())
}

fn view(
    person: PersonRecord,
    names: &BTreeMap<OrganizationId, String>,
    format: ResponseFormat,
) -> PersonView {
    let data = person.data;
    let organizations = data
        .organization_ids
        .iter()
        .map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_string()))
        .collect();
    let details = (format == ResponseFormat::Detailed).then_some(Details {
        phone: data.phone,
        department: data.department,
        organization_ids: data.organization_ids,
        created_at: person.created_at,
        created_by: person.created_by,
        updated_at: person.updated_at,
        updated_by: person.updated_by,
    });
    PersonView {
        id: person.id,
        name: format!("{} {}", data.first_name, data.last_name),
        email: data.email,
        job_title: data.job_title,
        organizations,
        version: person.version,
        details,
    }
}

fn path_of(found: &OrganizationName) -> String {
    let mut names = found.path.clone();
    names.push(found.name.clone());
    names.join(" › ")
}

fn email(raw: &str) -> Result<Email, ToolError> {
    Email::from_str(raw)
        .map_err(|error| ToolError::refused(format!("email {raw:?} is not valid: {error}")))
}

fn cursor(raw: &str) -> Result<Cursor, ToolError> {
    raw.parse()
        .map_err(|_| ToolError::refused("cursor is not a next_cursor from a previous answer"))
}

fn refusal(error: PersonError) -> ToolError {
    match error {
        PersonError::NotFound => {
            ToolError::refused("no such person, or not one you can see: search again")
        }
        PersonError::Stale(version) => ToolError::refused(format!(
            "the person changed since you read it and is now at version {version}: read it \
             again with people_search, then decide whether your change still applies"
        )),
        PersonError::NothingToChange => ToolError::refused("the call changes no field"),
        PersonError::Forbidden => ToolError::refused(
            "the person or one of the organizations is outside the ones you can change",
        ),
        PersonError::OrganizationsNotFound(_) => ToolError::refused(error.to_string()),
        PersonError::Db(error) => error.into(),
    }
}
