use std::collections::BTreeMap;
use std::str::FromStr;

use resysto_mcp::{Effect, ResponseFormat, Tool, ToolError, limit};
use resysto_model::OrganizationId;
use resysto_surrealdb::{Cursor, PageRequest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use super::model::OrganizationRecord;
use super::ops::{self, OrganizationError};
use super::store::{self, Named};

const BRANCHES: i64 = 50;

const SEARCH: &str = "Finds the organizations of the tenant: companies, their branches, \
departments, sites, all in one tree. Matches part of the name, ignoring case, and shows each \
organization with its place in the tree, such as \"Acme › Italia › Milano\". Call it to find an \
organization's id before people_search, people_add or org_overview, and to tell apart \
organizations with the same name. Answers the newest first, with a next_cursor when there are \
more.";

const OVERVIEW: &str = "Describes one organization: where it sits in the tree, its details, the \
branches right below it, and how many records of each kind belong to it, such as how many people. \
Name it by the id org_search gives, or by its exact name; a name that fits more than one \
organization is refused with the candidates.";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("org_search", Effect::Read, SEARCH, search),
        Tool::new("org_overview", Effect::Read, OVERVIEW, overview),
    ]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    /// Part of the name; leave it out to list every organization
    text: Option<String>,
    /// How many organizations to return: 20 when left out, at most 50
    limit: Option<u32>,
    /// The next_cursor of the previous answer, to read on
    cursor: Option<String>,
    #[serde(default)]
    response_format: ResponseFormat,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Overview {
    /// The organization: an id from org_search, or its exact name
    organization: String,
}

#[derive(Debug, Serialize)]
struct Found {
    organizations: Vec<OrganizationView>,
    next_cursor: Option<Cursor>,
}

#[derive(Debug, Serialize)]
struct OrganizationView {
    id: OrganizationId,
    path: String,
    active: bool,
    city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<OrganizationRecord>,
}

#[derive(Debug, Serialize)]
struct OverviewView {
    path: String,
    #[serde(flatten)]
    organization: OrganizationRecord,
    branches: Vec<Branch>,
    more_branches: bool,
    records: BTreeMap<String, i64>,
}

#[derive(Debug, Serialize)]
struct Branch {
    id: OrganizationId,
    name: String,
}

async fn search(db: Surreal<Any>, input: Search) -> Result<Found, ToolError> {
    let request = PageRequest {
        after: input.cursor.as_deref().map(cursor).transpose()?,
        limit: Some(limit(input.limit)),
    };
    let page = ops::search(&db, input.text.as_deref(), request)
        .await
        .map_err(refusal)?;
    let names = ancestor_names(&db, &page.data).await?;
    let organizations = page
        .data
        .into_iter()
        .map(|organization| OrganizationView {
            id: organization.id,
            path: path_of(&organization, &names),
            active: organization.data.active,
            city: organization.data.city.clone(),
            details: (input.response_format == ResponseFormat::Detailed).then_some(organization),
        })
        .collect();
    Ok(Found {
        organizations,
        next_cursor: page.next_cursor,
    })
}

async fn overview(db: Surreal<Any>, input: Overview) -> Result<OverviewView, ToolError> {
    let organization = match OrganizationId::from_str(&input.organization) {
        Ok(id) => ops::get(&db, id).await.map_err(refusal)?,
        Err(_) => only_named(&db, &input.organization).await?,
    };
    let names = ancestor_names(&db, std::slice::from_ref(&organization)).await?;
    let mut branches = store::branches(&db, organization.id, BRANCHES + 1).await?;
    let more_branches = branches.len() as i64 > BRANCHES;
    branches.truncate(usize::try_from(BRANCHES).unwrap_or_default());
    let records = store::uses(&db, organization.id).await?;
    Ok(OverviewView {
        path: path_of(&organization, &names),
        organization,
        branches: branches
            .into_iter()
            .map(|branch| Branch {
                id: branch.id,
                name: branch.name,
            })
            .collect(),
        more_branches,
        records,
    })
}

async fn only_named(db: &Surreal<Any>, name: &str) -> Result<OrganizationRecord, ToolError> {
    let mut named = store::named(db, name).await?;
    if named.len() > 1 {
        let names = ancestor_names(db, &named).await?;
        let candidates: Vec<String> = named
            .iter()
            .map(|found| format!("{} ({})", path_of(found, &names), found.id))
            .collect();
        return Err(ToolError::refused(format!(
            "{name:?} names {} organizations: {}; pass the id of the one you mean",
            named.len(),
            candidates.join(", ")
        )));
    }
    named.pop().ok_or_else(|| {
        ToolError::refused(format!(
            "no organization is named {name:?}: find it with org_search and pass its id"
        ))
    })
}

async fn ancestor_names(
    db: &Surreal<Any>,
    organizations: &[OrganizationRecord],
) -> Result<BTreeMap<OrganizationId, String>, ToolError> {
    let ids: Vec<OrganizationId> = organizations
        .iter()
        .flat_map(|organization| organization.data.path.iter().copied())
        .collect();
    let names: Vec<Named> = store::names(db, ids).await?;
    Ok(names
        .into_iter()
        .map(|named| (named.id, named.name))
        .collect())
}

// an ancestor outside what the session sees shows as its id
fn path_of(organization: &OrganizationRecord, names: &BTreeMap<OrganizationId, String>) -> String {
    let mut parts: Vec<String> = organization
        .data
        .path
        .iter()
        .map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_string()))
        .collect();
    parts.push(organization.data.name.clone());
    parts.join(" › ")
}

fn cursor(raw: &str) -> Result<Cursor, ToolError> {
    raw.parse()
        .map_err(|_| ToolError::refused("cursor is not a next_cursor from a previous answer"))
}

fn refusal(error: OrganizationError) -> ToolError {
    match error {
        OrganizationError::NotFound => {
            ToolError::refused("no such organization, or not one you can see: search again")
        }
        OrganizationError::Db(error) => error.into(),
        other => ToolError::refused(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use resysto_model::MemberId;
    use resysto_surrealdb::{Actor, Db, Grant, Via};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn an_overview_names_the_branches_and_counts_what_belongs_to_the_organization() {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, "t_acme")
            .await
            .unwrap();
        let [acme, milano, beta, beta_milano] = [(); 4].map(|()| OrganizationId::new());
        db.system_session("t_acme")
            .await
            .unwrap()
            .query(
                "CREATE $acme SET name = 'Acme', path = [];
                 CREATE $milano SET name = 'Milano', parent = $acme, path = [$acme];
                 CREATE $beta SET name = 'Beta', path = [];
                 CREATE $beta_milano SET name = 'Milano', parent = $beta, path = [$beta];
                 CREATE person SET first_name = 'Ada', last_name = 'Lovelace',
                     organization_ids = [$acme];",
            )
            .bind(("acme", acme))
            .bind(("milano", milano))
            .bind(("beta", beta))
            .bind(("beta_milano", beta_milano))
            .await
            .unwrap()
            .check()
            .unwrap();
        let actor = Actor {
            member: MemberId::new().into(),
            via: Via::Mcp,
            client: None,
            session: None,
            request: None,
            ip: None,
            user_agent: None,
        };
        let reading = Grant {
            organizations: None,
            write: false,
        };
        let session = db.record_session("t_acme", &actor, &reading).await.unwrap();
        let ask = |organization: String| -> Overview {
            serde_json::from_value(json!({ "organization": organization })).unwrap()
        };

        let shown = overview(session.clone(), ask("acme".into())).await.unwrap();

        let branches: Vec<&str> = shown
            .branches
            .iter()
            .map(|branch| branch.name.as_str())
            .collect();
        assert_eq!((shown.path.as_str(), branches), ("Acme", vec!["Milano"]));
        assert_eq!(shown.records.get("person"), Some(&1));
        let by_id = overview(session.clone(), ask(milano.to_string()))
            .await
            .unwrap();
        assert_eq!(by_id.path, "Acme › Milano");
        match overview(session, ask("Milano".into())).await {
            Err(ToolError::Refused(message)) => assert!(
                message.contains("Acme › Milano") && message.contains("Beta › Milano"),
                "{message}"
            ),
            other => panic!("an ambiguous name went through: {other:?}"),
        }
    }
}
