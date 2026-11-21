use axum::Extension;
use axum::http::{Method, StatusCode, header};
use axum_test::{TestResponse, TestServer};
use resysto_http::TenantAccess;
use resysto_model::OrganizationId;
use resysto_people::person;
use resysto_surrealdb::{Actor, Db, Grant, Via};
use serde_json::{Value, json};
use surrealdb::types::{RecordId, Uuid};

use crate::{TENANT, everything, tenant};

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

fn within(organization: OrganizationId) -> Grant {
    Grant {
        organizations: Some(vec![organization.into()]),
        write: true,
    }
}

// the router behind a server middleware that let a member with `grant` through
fn server(db: &Db, grant: Grant) -> TestServer {
    let access = TenantAccess {
        database: TENANT.into(),
        actor: Actor {
            member: RecordId::new("member", Uuid::new_v7()),
            via: Via::Ui,
            client: None,
            session: None,
            request: None,
            ip: None,
            user_agent: None,
        },
        grant,
    };
    let app = axum::Router::<Db>::from(person::router())
        .layer(Extension(access))
        .with_state(db.clone());
    TestServer::new(app)
}

fn ada(organizations: &[OrganizationId]) -> Value {
    json!({ "first_name": "Ada", "last_name": "Lovelace", "organization_ids": organizations })
}

fn etag(response: &TestResponse) -> String {
    response.header(header::ETAG).to_str().unwrap().to_owned()
}

// the path of the new person
async fn create(server: &TestServer, organizations: &[OrganizationId]) -> String {
    let created = server.post("/").json(&ada(organizations)).await;
    created.assert_status(StatusCode::CREATED);
    let created: Value = created.json();
    format!("/{}", created["data"]["id"].as_str().unwrap())
}

#[tokio::test]
async fn a_write_needs_the_etag_of_the_version_it_changes() {
    let db = tenant().await;
    let acme = organization(&db).await;
    let server = server(&db, everything());
    let created = server.post("/").json(&ada(&[acme])).await;
    let read = etag(&created);
    let created: Value = created.json();
    let person = format!("/{}", created["data"]["id"].as_str().unwrap());
    assert_eq!(read, format!("\"{}\"", created["data"]["version"]));

    let ciso = json!({ "job_title": "CISO" });
    let unconditional = server.patch(&person).json(&ciso).await;
    unconditional.assert_status(StatusCode::PRECONDITION_REQUIRED);
    let changed = server
        .patch(&person)
        .add_header(header::IF_MATCH, &read)
        .json(&ciso)
        .await;
    changed.assert_status_ok();
    let current = etag(&changed);
    assert_ne!(current, read);

    // a second writer that read the person before the first one wrote
    let late = server
        .patch(&person)
        .add_header(header::IF_MATCH, &read)
        .json(&json!({ "job_title": "CTO" }))
        .await;
    late.assert_status(StatusCode::PRECONDITION_FAILED);

    let deleted = server
        .delete(&person)
        .add_header(header::IF_MATCH, &current)
        .await;
    deleted.assert_status_no_content();
    server.get(&person).await.assert_status_not_found();
}

#[tokio::test]
async fn a_patch_that_is_not_one_clean_change_is_refused() {
    let db = tenant().await;
    let acme = organization(&db).await;
    let server = server(&db, everything());
    let person = create(&server, &[acme]).await;
    let read = etag(&server.get(&person).await);

    let refusals = [
        (read.as_str(), json!(["Bob"])),
        (read.as_str(), json!({})),
        (read.as_str(), json!({ "phnoe": "06 123" })),
        (read.trim_matches('"'), json!({ "job_title": "CISO" })),
    ];
    for (if_match, body) in refusals {
        let answer = server
            .patch(&person)
            .add_header(header::IF_MATCH, if_match)
            .json(&body)
            .await;
        assert_eq!(
            answer.status_code(),
            StatusCode::BAD_REQUEST,
            "{if_match} {body}: {}",
            answer.text()
        );
    }
    assert_eq!(etag(&server.get(&person).await), read);
}

#[tokio::test]
async fn the_refusals_of_the_rules_reach_the_client_as_their_status() {
    let db = tenant().await;
    let milano = organization(&db).await;
    let roma = organization(&db).await;
    let all = server(&db, everything());
    let in_roma = create(&all, &[roma]).await;
    let in_both = create(&all, &[milano, roma]).await;
    let scoped = server(&db, within(milano));

    let refusals = [
        (Method::GET, in_roma.as_str(), None, StatusCode::NOT_FOUND),
        (
            Method::POST,
            "/",
            Some(ada(&[roma])),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::PATCH,
            &in_both,
            Some(json!({ "job_title": "CISO" })),
            StatusCode::FORBIDDEN,
        ),
        (Method::DELETE, &in_both, None, StatusCode::FORBIDDEN),
    ];
    for (method, uri, body, expected) in refusals {
        // a write made, as a client makes it, on the version it just read
        let read = all.get(uri).await.maybe_header(header::ETAG);
        let mut request = scoped.method(method.clone(), uri);
        if let Some(read) = read {
            request = request.add_header(header::IF_MATCH, read);
        }
        if let Some(body) = &body {
            request = request.json(body);
        }
        let answer = request.await;
        assert_eq!(
            answer.status_code(),
            expected,
            "{method} {uri}: {}",
            answer.text()
        );
    }
}
