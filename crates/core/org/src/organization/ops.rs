use std::collections::{BTreeMap, BTreeSet};

use resysto_model::OrganizationId;
use resysto_surrealdb::{Page, PageRequest, Tx, transaction};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use super::model::{MAX_DEPTH, NewOrganization, OrganizationPatch, OrganizationRecord};
use super::store;

#[derive(Debug, thiserror::Error)]
pub enum OrganizationError {
    #[error("organization not found")]
    NotFound,
    #[error("the organization changed since it was read: its version is now {0}")]
    Stale(i64),
    #[error("the patch changes no field")]
    NothingToChange,
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
    let organization = input.into_organization(path);
    store::insert(db, OrganizationId::new(), organization)
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

/// One page of the organizations, newest first
pub async fn list(
    db: &Surreal<Any>,
    request: PageRequest,
) -> Result<Page<OrganizationRecord>, OrganizationError> {
    Ok(store::list(db, request).await?)
}

/// Applies `patch`, if the organization is still at `version`; a new parent moves the whole
/// branch below the organization with it
pub async fn update(
    db: &Surreal<Any>,
    id: OrganizationId,
    version: i64,
    patch: OrganizationPatch,
) -> Result<OrganizationRecord, OrganizationError> {
    if patch == OrganizationPatch::default() {
        return Err(OrganizationError::NothingToChange);
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
    id: OrganizationId,
    version: i64,
    patch: OrganizationPatch,
) -> Result<Result<OrganizationRecord, OrganizationError>, resysto_surrealdb::Error> {
    let Some(record) = store::find(tx, id).await? else {
        return Ok(Err(OrganizationError::NotFound));
    };
    if record.version != version {
        return Ok(Err(OrganizationError::Stale(record.version)));
    }
    let mut organization = record.data;
    let old_path = organization.path.clone();

    if let Some(parent) = patch.parent
        && parent != organization.parent
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
        organization.parent = parent;
        organization.path = path;
    }

    patch.apply(&mut organization);
    Ok(store::update(tx, id, organization)
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

/// Deletes an organization that has no children and no records, if it is still at `version`
pub async fn delete(
    db: &Surreal<Any>,
    id: OrganizationId,
    version: i64,
) -> Result<(), OrganizationError> {
    match transaction(db, |tx| async move { delete_in(&tx, id, version).await }).await {
        Ok(outcome) => outcome,
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

async fn delete_in(
    tx: &Tx,
    id: OrganizationId,
    version: i64,
) -> Result<Result<(), OrganizationError>, resysto_surrealdb::Error> {
    let Some(record) = store::find(tx, id).await? else {
        return Ok(Err(OrganizationError::NotFound));
    };
    if record.version != version {
        return Ok(Err(OrganizationError::Stale(record.version)));
    }
    if store::has_children(tx, id).await? {
        return Ok(Err(OrganizationError::HasChildren));
    }
    let uses = store::uses(tx, id).await?;
    if !uses.is_empty() {
        return Ok(Err(OrganizationError::InUse(uses)));
    }
    Ok(if store::delete(tx, id).await? {
        Ok(())
    } else {
        Err(OrganizationError::Forbidden)
    })
}

fn uses_text(uses: &BTreeMap<String, i64>) -> String {
    let parts: Vec<String> = uses
        .iter()
        .map(|(table, count)| format!("{count} {table}"))
        .collect();
    parts.join(", ")
}
