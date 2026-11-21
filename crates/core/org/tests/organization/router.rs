use axum::Extension;
use axum::http::{Method, StatusCode, header};
use axum_test::{TestResponse, TestServer};
use resysto_http::TenantAccess;
use resysto_model::OrganizationId;
use resysto_org::organization;
use resysto_surrealdb::{Actor, Db, Grant, Via};
use serde_json::{Value, json};
use surrealdb::types::{RecordId, Uuid};

use crate::{TENANT, tenant};

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
    let app = axum::Router::<Db>::from(organization::router())
        .layer(Extension(access))
        .with_state(db.clone());
    TestServer::new(app)
}

fn etag(response: &TestResponse) -> String {
    response.header(header::ETAG).to_str().unwrap().to_owned()
}

async fn add(server: &TestServer, parent: Option<OrganizationId>) -> OrganizationId {
    let created = server
        .post("/")
        .json(&json!({ "name": "acme", "parent": parent }))
        .await;
    created.assert_status(StatusCode::CREATED);
    let created: Value = created.json();
    serde_json::from_value(created["data"]["id"].clone()).unwrap()
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
async fn a_write_needs_the_etag_of_the_version_it_changes() {
    let db = tenant().await;
    let server = server(&db, everything());
    let created = server.post("/").json(&json!({ "name": "acme" })).await;
    let read = etag(&created);
    let created: Value = created.json();
    let uri = format!("/{}", created["data"]["id"].as_str().unwrap());
    assert_eq!(read, format!("\"{}\"", created["data"]["version"]));

    let renamed = json!({ "name": "acme spa" });
    let unconditional = server.patch(&uri).json(&renamed).await;
    unconditional.assert_status(StatusCode::PRECONDITION_REQUIRED);
    let nothing = server
        .patch(&uri)
        .add_header(header::IF_MATCH, &read)
        .json(&json!({}))
        .await;
    nothing.assert_status_bad_request();
    let changed = server
        .patch(&uri)
        .add_header(header::IF_MATCH, &read)
        .json(&renamed)
        .await;
    changed.assert_status_ok();
    let current = etag(&changed);
    assert_ne!(current, read);

    // a second writer that read the organization before the first one wrote
    let late = server
        .patch(&uri)
        .add_header(header::IF_MATCH, &read)
        .json(&json!({ "name": "acme srl" }))
        .await;
    late.assert_status(StatusCode::PRECONDITION_FAILED);

    let deleted = server
        .delete(&uri)
        .add_header(header::IF_MATCH, &current)
        .await;
    deleted.assert_status_no_content();
    server.get(&uri).await.assert_status_not_found();
}

#[tokio::test]
async fn the_refusals_of_the_rules_reach_the_client_as_their_status() {
    let db = tenant().await;
    let all = server(&db, everything());
    let root = add(&all, None).await;
    let branch = add(&all, Some(root)).await;
    let scoped = server(&db, within(&[branch]));

    let refusals = [
        (
            &all,
            Method::POST,
            "/".to_owned(),
            Some(json!({ "name": "orphan", "parent": OrganizationId::new() })),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            &all,
            Method::DELETE,
            format!("/{root}"),
            None,
            StatusCode::CONFLICT,
        ),
        (
            &all,
            Method::GET,
            format!("/{}", OrganizationId::new()),
            None,
            StatusCode::NOT_FOUND,
        ),
        (
            &scoped,
            Method::PATCH,
            format!("/{branch}"),
            Some(json!({ "name": "sede" })),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (server, method, uri, body, expected) in refusals {
        // a write made, as a client makes it, on the version it just read
        let read = all.get(&uri).await.maybe_header(header::ETAG);
        let mut request = server.method(method.clone(), &uri);
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

#[tokio::test]
async fn a_reassign_answers_how_many_records_moved() {
    let db = tenant().await;
    let server = server(&db, everything());
    let x = add(&server, None).await;
    let y = add(&server, None).await;
    tag(&db, &[x]).await;

    let moved = server
        .post(&format!("/{x}/reassign"))
        .json(&json!({ "to": y }))
        .await;

    moved.assert_status_ok();
    let moved: Value = moved.json();
    assert_eq!(moved["data"]["reassigned"], 1);
}
