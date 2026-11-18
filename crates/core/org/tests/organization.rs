#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use resysto_http::TenantAccess;
use resysto_model::OrganizationId;
use resysto_org::organization::{self, MAX_DEPTH};
use resysto_surrealdb::{Db, Grant};
use serde_json::{Value, json};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, Uuid};
use tower::ServiceExt;

const TENANT: &str = "t_acme";

// a tenant plus a table that tags its rows like every bounded context does
async fn tenant() -> Db {
    let db = Db::memory().await.unwrap();
    resysto_migrator::provision_tenant(&db, TENANT)
        .await
        .unwrap();
    system(&db)
        .await
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

async fn system(db: &Db) -> Surreal<Any> {
    db.system_session(TENANT).await.unwrap()
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

// a request that the server's middleware already authenticated with `grant`; the answer's ETag too
async fn send(
    db: &Db,
    grant: &Grant,
    method: Method,
    uri: &str,
    if_match: Option<&str>,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    let access = TenantAccess {
        database: TENANT.into(),
        account: RecordId::new("account", Uuid::new_v7()),
        grant: grant.clone(),
    };
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(access);
    if let Some(if_match) = if_match {
        request = request.header(header::IF_MATCH, if_match);
    }
    let request = request.body(Body::from(body.to_string())).unwrap();
    let response = organization::router()
        .with_state(db.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .map(|etag| etag.to_str().unwrap().to_owned());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, etag, body)
}

async fn call(
    db: &Db,
    grant: &Grant,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (status, _, body) = send(db, grant, method, uri, None, body).await;
    (status, body)
}

// a write made, as a client makes it, on the version it just read
async fn write(
    db: &Db,
    grant: &Grant,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (_, etag, _) = send(db, &everything(), Method::GET, uri, None, Value::Null).await;
    let (status, _, body) = send(db, grant, method, uri, etag.as_deref(), body).await;
    (status, body)
}

async fn add(db: &Db, parent: Option<OrganizationId>) -> OrganizationId {
    let body = json!({ "name": "acme", "parent": parent });
    let (status, created) = call(db, &everything(), Method::POST, "/", body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    serde_json::from_value(created["data"]["id"].clone()).unwrap()
}

async fn read(db: &Db, id: OrganizationId) -> Value {
    let (status, read) = call(
        db,
        &everything(),
        Method::GET,
        &format!("/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    read["data"].clone()
}

async fn path_of(db: &Db, id: OrganizationId) -> Vec<OrganizationId> {
    serde_json::from_value(read(db, id).await["path"].clone()).unwrap()
}

async fn move_to(
    db: &Db,
    id: OrganizationId,
    parent: Option<OrganizationId>,
) -> (StatusCode, Value) {
    let body = json!({ "parent": parent });
    write(db, &everything(), Method::PATCH, &format!("/{id}"), body).await
}

async fn tag(db: &Db, organizations: &[OrganizationId]) {
    system(db)
        .await
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
    let orphan = json!({ "name": "orphan", "parent": OrganizationId::new() });
    let (status, _) = call(&db, &everything(), Method::POST, "/", orphan).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let mut deepest = add(&db, None).await;
    for _ in 0..MAX_DEPTH {
        deepest = add(&db, Some(deepest)).await;
    }
    assert_eq!(path_of(&db, deepest).await.len(), MAX_DEPTH);

    let too_deep = json!({ "name": "too deep", "parent": deepest });
    let (status, _) = call(&db, &everything(), Method::POST, "/", too_deep).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn moving_a_branch_rewrites_the_paths_below_it() {
    let db = tenant().await;
    let r1 = add(&db, None).await;
    let a = add(&db, Some(r1)).await;
    let b = add(&db, Some(a)).await;
    let c = add(&db, Some(b)).await;
    let r2 = add(&db, None).await;

    let (status, moved) = move_to(&db, a, Some(r2)).await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["data"]["parent"], json!(r2));
    assert_eq!(path_of(&db, a).await, vec![r2]);
    assert_eq!(path_of(&db, b).await, vec![r2, a]);
    assert_eq!(path_of(&db, c).await, vec![r2, a, b]);

    let (status, moved) = move_to(&db, a, None).await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["data"]["parent"], Value::Null);
    assert!(path_of(&db, a).await.is_empty());
    assert_eq!(path_of(&db, b).await, vec![a]);
    assert_eq!(path_of(&db, c).await, vec![a, b]);
}

#[tokio::test]
async fn a_move_that_would_break_the_tree_changes_nothing() {
    let db = tenant().await;
    let root = add(&db, None).await;
    let a = add(&db, Some(root)).await;
    let b = add(&db, Some(a)).await;
    let mut deep = add(&db, None).await;
    for _ in 1..MAX_DEPTH {
        deep = add(&db, Some(deep)).await;
    }

    let refusals = [
        (a, "own parent"),
        (b, "under its child"),
        (deep, "b would sit six levels down"),
        (OrganizationId::new(), "missing parent"),
    ];
    for (parent, case) in refusals {
        let (status, answer) = move_to(&db, a, Some(parent)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{case}: {answer}");
    }
    assert_eq!(path_of(&db, b).await, vec![root, a]);

    // one level up the same branch fits exactly
    let above_deep = path_of(&db, deep).await.last().copied();
    let (status, moved) = move_to(&db, a, above_deep).await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(path_of(&db, b).await.len(), MAX_DEPTH);
}

#[tokio::test]
async fn updating_a_missing_organization_creates_nothing() {
    let db = tenant().await;
    let ghost = format!("/{}", OrganizationId::new());

    let (status, _, _) = send(
        &db,
        &everything(),
        Method::PATCH,
        &ghost,
        Some("\"1\""),
        json!({ "name": "ghost" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(&db, &everything(), Method::GET, &ghost, Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_an_organization_without_children_or_records_is_deleted() {
    let db = tenant().await;
    let parent = add(&db, None).await;
    let child = add(&db, Some(parent)).await;
    let other = add(&db, None).await;
    tag(&db, &[child]).await;
    tag(&db, &[child, other]).await;
    let all = everything();

    for (id, case) in [(parent, "has children"), (child, "has records")] {
        let (status, answer) =
            write(&db, &all, Method::DELETE, &format!("/{id}"), Value::Null).await;
        assert_eq!(status, StatusCode::CONFLICT, "{case}: {answer}");
    }

    let to_other = json!({ "to": other });
    let (status, _) = call(
        &db,
        &all,
        Method::POST,
        &format!("/{child}/reassign"),
        to_other,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for id in [child, parent] {
        let (status, answer) =
            write(&db, &all, Method::DELETE, &format!("/{id}"), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{id}: {answer}");
    }
    let gone = format!("/{parent}");
    let (status, _, _) = send(&db, &all, Method::DELETE, &gone, Some("\"1\""), Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_reassign_goes_to_another_organization_that_exists() {
    let db = tenant().await;
    let x = add(&db, None).await;
    let y = add(&db, None).await;
    let ghost = OrganizationId::new();
    tag(&db, &[x]).await;
    let all = everything();

    let refusals = [
        (x, x, StatusCode::UNPROCESSABLE_ENTITY),
        (x, ghost, StatusCode::UNPROCESSABLE_ENTITY),
        (ghost, y, StatusCode::NOT_FOUND),
    ];
    for (from, to, expected) in refusals {
        let uri = format!("/{from}/reassign");
        let (status, answer) = call(&db, &all, Method::POST, &uri, json!({ "to": to })).await;
        assert_eq!(status, expected, "{from} to {to}: {answer}");
    }

    let uri = format!("/{x}/reassign");
    let (status, moved) = call(&db, &all, Method::POST, &uri, json!({ "to": y })).await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["data"]["reassigned"], 1);
}

#[tokio::test]
async fn a_scoped_member_sees_its_branches_and_changes_no_organization() {
    let db = tenant().await;
    let root = add(&db, None).await;
    let milano = add(&db, Some(root)).await;
    let sede = add(&db, Some(milano)).await;
    let roma = add(&db, Some(root)).await;
    let ghost = OrganizationId::new();

    let reached = organization::with_branches(&system(&db).await, vec![milano, ghost])
        .await
        .unwrap();
    assert_eq!(reached, vec![milano, ghost, sede]);
    let scoped = within(&reached);

    let (_, listed) = call(&db, &scoped, Method::GET, "/", Value::Null).await;
    let seen: Vec<OrganizationId> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|organization| serde_json::from_value(organization["id"].clone()).unwrap())
        .collect();
    assert_eq!(seen, vec![sede, milano]);

    let sede_uri = format!("/{sede}");
    let refusals = [
        (
            Method::GET,
            format!("/{roma}"),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/".to_owned(),
            json!({ "name": "ufficio", "parent": sede }),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::PATCH,
            sede_uri.clone(),
            json!({ "name": "sede nuova" }),
            StatusCode::FORBIDDEN,
        ),
        (Method::DELETE, sede_uri, Value::Null, StatusCode::FORBIDDEN),
    ];
    for (method, uri, body, expected) in refusals {
        let (status, answer) = write(&db, &scoped, method.clone(), &uri, body).await;
        assert_eq!(status, expected, "{method} {uri}: {answer}");
    }
    assert_eq!(read(&db, sede).await["name"], "acme");
}

#[tokio::test]
async fn a_write_on_a_version_already_changed_is_refused_and_changes_nothing() {
    let db = tenant().await;
    let id = add(&db, None).await;
    let uri = format!("/{id}");
    let all = everything();
    let (_, read_before, _) = send(&db, &all, Method::GET, &uri, None, Value::Null).await;
    let read_before = read_before.unwrap();

    let renamed = json!({ "name": "acme spa" });
    let (status, _, _) = send(&db, &all, Method::PATCH, &uri, Some(&read_before), renamed).await;
    assert_eq!(status, StatusCode::OK);

    let stale = [
        (Method::PATCH, json!({ "name": "acme srl" })),
        (Method::DELETE, Value::Null),
    ];
    for (method, body) in stale {
        let (status, _, answer) =
            send(&db, &all, method.clone(), &uri, Some(&read_before), body).await;
        assert_eq!(
            status,
            StatusCode::PRECONDITION_FAILED,
            "{method}: {answer}"
        );
    }
    let (_, current, _) = send(&db, &all, Method::GET, &uri, None, Value::Null).await;
    let nothing = json!({});
    let (status, _, answer) =
        send(&db, &all, Method::PATCH, &uri, current.as_deref(), nothing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(read(&db, id).await["name"], "acme spa");
}
