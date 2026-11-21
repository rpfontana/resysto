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
pub(super) async fn search(
    db: &Surreal<Any>,
    text: Option<&str>,
    request: PageRequest,
) -> Result<Page<OrganizationRecord>, OrganizationError> {
    let text = text
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);
    Ok(store::search(db, text, request).await?)
}

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

#[cfg(test)]
mod tests {
    use resysto_model::MemberId;
    use resysto_surrealdb::{Actor, Db, Grant, Via};
    use surrealdb::types::RecordId;

    use super::*;

    const TENANT: &str = "t_acme";

    // a tenant plus a table that tags its rows like every bounded context does
    async fn tenant() -> Db {
        let db = Db::memory().await.unwrap();
        resysto_migrator::provision_tenant(&db, TENANT)
            .await
            .unwrap();
        db.system_session(TENANT)
            .await
            .unwrap()
            .query(
                "DEFINE TABLE item SCHEMAFULL PERMISSIONS FULL;
                 DEFINE FIELD organization_ids ON item TYPE array<record<organization>>
                     REFERENCE ON DELETE REJECT ASSERT array::len($value) > 0;",
            )
            .await
            .unwrap()
            .check()
            .unwrap();
        db
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

    fn within(organizations: &[OrganizationId]) -> Grant {
        Grant {
            organizations: Some(organizations.iter().copied().map(RecordId::from).collect()),
            write: true,
        }
    }

    fn named(name: &str, parent: Option<OrganizationId>) -> NewOrganization {
        NewOrganization {
            name: name.into(),
            parent,
            ..NewOrganization::default()
        }
    }

    fn renamed(name: &str) -> OrganizationPatch {
        OrganizationPatch {
            name: Some(name.into()),
            ..OrganizationPatch::default()
        }
    }

    async fn add(db: &Surreal<Any>, parent: Option<OrganizationId>) -> OrganizationId {
        create(db, named("acme", parent)).await.unwrap().id
    }

    async fn version_of(db: &Surreal<Any>, id: OrganizationId) -> i64 {
        get(db, id).await.unwrap().version
    }

    async fn path_of(db: &Surreal<Any>, id: OrganizationId) -> Vec<OrganizationId> {
        get(db, id).await.unwrap().data.path
    }

    // a move made on the version just read
    async fn move_to(
        db: &Surreal<Any>,
        id: OrganizationId,
        parent: Option<OrganizationId>,
    ) -> Result<OrganizationRecord, OrganizationError> {
        let patch = OrganizationPatch {
            parent: Some(parent),
            ..OrganizationPatch::default()
        };
        update(db, id, version_of(db, id).await, patch).await
    }

    async fn tag(db: &Db, organizations: &[OrganizationId]) {
        db.system_session(TENANT)
            .await
            .unwrap()
            .query("CREATE item SET organization_ids = $ids")
            .bind(("ids", organizations.to_vec()))
            .await
            .unwrap()
            .check()
            .unwrap();
    }

    #[tokio::test]
    async fn a_parent_must_exist_and_the_tree_stops_five_levels_down() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;

        let orphan = create(&all, named("orphan", Some(OrganizationId::new()))).await;
        assert!(
            matches!(orphan, Err(OrganizationError::ParentNotFound)),
            "{orphan:?}"
        );

        let mut deepest = add(&all, None).await;
        for _ in 0..MAX_DEPTH {
            deepest = add(&all, Some(deepest)).await;
        }
        assert_eq!(path_of(&all, deepest).await.len(), MAX_DEPTH);
        let too_deep = create(&all, named("too deep", Some(deepest))).await;
        assert!(
            matches!(too_deep, Err(OrganizationError::TooDeep)),
            "{too_deep:?}"
        );
    }

    #[tokio::test]
    async fn moving_a_branch_rewrites_the_paths_below_it() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let r1 = add(&all, None).await;
        let a = add(&all, Some(r1)).await;
        let b = add(&all, Some(a)).await;
        let c = add(&all, Some(b)).await;
        let r2 = add(&all, None).await;

        let moved = move_to(&all, a, Some(r2)).await.unwrap();
        assert_eq!(moved.data.parent, Some(r2));
        assert_eq!(path_of(&all, a).await, [r2]);
        assert_eq!(path_of(&all, b).await, [r2, a]);
        assert_eq!(path_of(&all, c).await, [r2, a, b]);

        let moved = move_to(&all, a, None).await.unwrap();
        assert_eq!(moved.data.parent, None);
        assert!(path_of(&all, a).await.is_empty());
        assert_eq!(path_of(&all, b).await, [a]);
        assert_eq!(path_of(&all, c).await, [a, b]);
    }

    #[tokio::test]
    async fn a_move_that_would_break_the_tree_changes_nothing() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let root = add(&all, None).await;
        let a = add(&all, Some(root)).await;
        let b = add(&all, Some(a)).await;
        let mut deep = add(&all, None).await;
        for _ in 1..MAX_DEPTH {
            deep = add(&all, Some(deep)).await;
        }

        let own = move_to(&all, a, Some(a)).await;
        assert!(matches!(own, Err(OrganizationError::OwnParent)), "{own:?}");
        let under_child = move_to(&all, a, Some(b)).await;
        assert!(
            matches!(under_child, Err(OrganizationError::Cycle)),
            "{under_child:?}"
        );
        // b would sit six levels down
        let too_deep = move_to(&all, a, Some(deep)).await;
        assert!(
            matches!(too_deep, Err(OrganizationError::TooDeep)),
            "{too_deep:?}"
        );
        let missing = move_to(&all, a, Some(OrganizationId::new())).await;
        assert!(
            matches!(missing, Err(OrganizationError::ParentNotFound)),
            "{missing:?}"
        );
        assert_eq!(path_of(&all, b).await, [root, a]);

        // one level up the same branch fits exactly
        let above_deep = path_of(&all, deep).await.last().copied();
        move_to(&all, a, above_deep).await.unwrap();
        assert_eq!(path_of(&all, b).await.len(), MAX_DEPTH);
    }

    #[tokio::test]
    async fn updating_a_missing_organization_creates_nothing() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let ghost = OrganizationId::new();

        let updated = update(&all, ghost, 1, renamed("ghost")).await;
        assert!(
            matches!(updated, Err(OrganizationError::NotFound)),
            "{updated:?}"
        );
        let read = get(&all, ghost).await;
        assert!(matches!(read, Err(OrganizationError::NotFound)), "{read:?}");
    }

    #[tokio::test]
    async fn only_an_organization_without_children_or_records_is_deleted() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let parent = add(&all, None).await;
        let child = add(&all, Some(parent)).await;
        let other = add(&all, None).await;
        tag(&db, &[child]).await;
        tag(&db, &[child, other]).await;

        let with_child = delete(&all, parent, version_of(&all, parent).await).await;
        assert!(
            matches!(with_child, Err(OrganizationError::HasChildren)),
            "{with_child:?}"
        );
        let with_records = delete(&all, child, version_of(&all, child).await).await;
        assert!(
            matches!(&with_records, Err(OrganizationError::InUse(uses)) if uses.get("item") == Some(&2)),
            "{with_records:?}"
        );

        reassign(&all, child, other).await.unwrap();
        for id in [child, parent] {
            delete(&all, id, version_of(&all, id).await).await.unwrap();
        }
        let gone = delete(&all, parent, 1).await;
        assert!(matches!(gone, Err(OrganizationError::NotFound)), "{gone:?}");
    }

    #[tokio::test]
    async fn a_reassign_goes_to_another_organization_that_exists() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let x = add(&all, None).await;
        let y = add(&all, None).await;
        let ghost = OrganizationId::new();
        tag(&db, &[x]).await;

        let to_itself = reassign(&all, x, x).await;
        assert!(
            matches!(to_itself, Err(OrganizationError::SameTarget)),
            "{to_itself:?}"
        );
        let to_ghost = reassign(&all, x, ghost).await;
        assert!(
            matches!(to_ghost, Err(OrganizationError::TargetNotFound)),
            "{to_ghost:?}"
        );
        let from_ghost = reassign(&all, ghost, y).await;
        assert!(
            matches!(from_ghost, Err(OrganizationError::NotFound)),
            "{from_ghost:?}"
        );

        assert_eq!(reassign(&all, x, y).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_scoped_member_sees_its_branches_and_changes_no_organization() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let root = add(&all, None).await;
        let milano = add(&all, Some(root)).await;
        let sede = add(&all, Some(milano)).await;
        let roma = add(&all, Some(root)).await;
        let ghost = OrganizationId::new();

        // the server widens a member's organizations on a system session, before its own exists
        let system = db.system_session(TENANT).await.unwrap();
        let reached = with_branches(&system, vec![milano, ghost]).await.unwrap();
        assert_eq!(reached, [milano, ghost, sede]);
        let scoped = session(&db, &within(&reached)).await;

        let listed = list(&scoped, PageRequest::default()).await.unwrap();
        let seen: Vec<OrganizationId> = listed.data.iter().map(|found| found.id).collect();
        assert_eq!(seen, [sede, milano]);

        let outside = get(&scoped, roma).await;
        assert!(
            matches!(outside, Err(OrganizationError::NotFound)),
            "{outside:?}"
        );
        let below = create(&scoped, named("ufficio", Some(sede))).await;
        assert!(
            matches!(below, Err(OrganizationError::Forbidden)),
            "{below:?}"
        );
        let version = version_of(&scoped, sede).await;
        let changed = update(&scoped, sede, version, renamed("sede nuova")).await;
        assert!(
            matches!(changed, Err(OrganizationError::Forbidden)),
            "{changed:?}"
        );
        let deleted = delete(&scoped, sede, version).await;
        assert!(
            matches!(deleted, Err(OrganizationError::Forbidden)),
            "{deleted:?}"
        );
        assert_eq!(get(&all, sede).await.unwrap().data.name, "acme");
    }

    #[tokio::test]
    async fn a_write_on_a_version_already_changed_is_refused_and_changes_nothing() {
        let db = tenant().await;
        let all = session(&db, &everything()).await;
        let id = add(&all, None).await;
        let read = version_of(&all, id).await;
        update(&all, id, read, renamed("acme spa")).await.unwrap();

        // a second writer that read the organization before the first one wrote
        let second = update(&all, id, read, renamed("acme srl")).await;
        assert!(
            matches!(second, Err(OrganizationError::Stale(_))),
            "{second:?}"
        );
        let deleted = delete(&all, id, read).await;
        assert!(
            matches!(deleted, Err(OrganizationError::Stale(_))),
            "{deleted:?}"
        );
        assert_eq!(get(&all, id).await.unwrap().data.name, "acme spa");
    }
}
