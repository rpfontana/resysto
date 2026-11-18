#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use resysto_http::TenantAccess;
use resysto_model::OrganizationId;
use resysto_people::person;
use resysto_surrealdb::{Db, Grant};
use serde_json::{Value, json};
use surrealdb::types::{RecordId, Uuid};
use tower::ServiceExt;

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

fn ada(organizations: &[OrganizationId]) -> Value {
    json!({ "first_name": "Ada", "last_name": "Lovelace", "organization_ids": organizations })
}

// a request that the server's middleware already authenticated with `grant`
async fn call(
    db: &Db,
    grant: &Grant,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let access = TenantAccess {
        database: TENANT.into(),
        account: RecordId::new("account", Uuid::new_v7()),
        grant: grant.clone(),
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(access)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = person::router()
        .with_state(db.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

// the path of the new person
async fn create(db: &Db, organizations: &[OrganizationId]) -> String {
    let (status, created) = call(db, &everything(), Method::POST, "/", ada(organizations)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    format!("/{}", created["data"]["id"].as_str().unwrap())
}

#[tokio::test]
async fn a_person_belongs_only_to_organizations_that_exist() {
    let db = tenant().await;
    let acme = organization(&db).await;
    let ghost = OrganizationId::new();
    let all = everything();

    let (status, _) = call(&db, &all, Method::POST, "/", ada(&[acme, ghost])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let person = create(&db, &[acme, acme]).await;
    let (status, _) = call(
        &db,
        &all,
        Method::PATCH,
        &person,
        json!({ "organization_ids": [ghost] }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, read) = call(&db, &all, Method::GET, &person, Value::Null).await;
    assert_eq!(read["data"]["organization_ids"], json!([acme]));
}

#[tokio::test]
async fn a_scoped_member_reads_and_writes_only_the_people_of_its_organizations() {
    let db = tenant().await;
    let milano = organization(&db).await;
    let roma = organization(&db).await;
    let in_milano = create(&db, &[milano]).await;
    let in_roma = create(&db, &[roma]).await;
    let in_both = create(&db, &[milano, roma]).await;
    let scoped = within(milano, true);

    let (_, listed) = call(&db, &scoped, Method::GET, "/", Value::Null).await;
    let seen: Vec<String> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|person| format!("/{}", person["id"].as_str().unwrap()))
        .collect();
    assert_eq!(seen, [in_milano, in_both.clone()]);

    let refusals = [
        (
            Method::GET,
            in_roma.as_str(),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        // an organization outside the scope reads as one that does not exist
        (
            Method::POST,
            "/",
            ada(&[roma]),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::PATCH,
            &in_both,
            json!({ "job_title": "CISO" }),
            StatusCode::FORBIDDEN,
        ),
        (Method::DELETE, &in_both, Value::Null, StatusCode::FORBIDDEN),
    ];
    for (method, uri, body, expected) in refusals {
        let (status, answer) = call(&db, &scoped, method.clone(), uri, body).await;
        assert_eq!(status, expected, "{method} {uri}: {answer}");
    }
    let (_, kept) = call(&db, &everything(), Method::GET, &in_both, Value::Null).await;
    assert_eq!(kept["data"]["job_title"], Value::Null);

    let reading = within(milano, false);
    let (status, _) = call(&db, &reading, Method::POST, "/", ada(&[milano])).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
