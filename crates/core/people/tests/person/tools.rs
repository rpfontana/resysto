use resysto_mcp::ToolError;
use resysto_model::{MemberId, OrganizationId};
use resysto_people::person;
use resysto_surrealdb::{Actor, Via};
use serde_json::{Value, json};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::{TENANT, everything, tenant};

struct Agent {
    session: Surreal<Any>,
    acme: OrganizationId,
    acme_milano: OrganizationId,
}

// Acme and Beta, each with a branch called Milano, and an agent's session that reaches all of them
async fn agent() -> Agent {
    let db = tenant().await;
    let [acme, acme_milano, beta, beta_milano] = [(); 4].map(|()| OrganizationId::new());
    db.system_session(TENANT)
        .await
        .unwrap()
        .query(
            "CREATE $acme SET name = 'Acme', path = [];
             CREATE $acme_milano SET name = 'Milano', parent = $acme, path = [$acme];
             CREATE $beta SET name = 'Beta', path = [];
             CREATE $beta_milano SET name = 'Milano', parent = $beta, path = [$beta];",
        )
        .bind(("acme", acme))
        .bind(("acme_milano", acme_milano))
        .bind(("beta", beta))
        .bind(("beta_milano", beta_milano))
        .await
        .unwrap()
        .check()
        .unwrap();
    let actor = Actor {
        member: MemberId::new().into(),
        via: Via::Mcp,
        client: Some("https://claude.ai/oauth/claude-code-client-metadata".into()),
        session: None,
        request: None,
        ip: None,
        user_agent: None,
    };
    let session = db
        .record_session(TENANT, &actor, &everything())
        .await
        .unwrap();
    Agent {
        session,
        acme,
        acme_milano,
    }
}

// what the tool called `name` answers, as the server's MCP endpoint calls it
async fn call(agent: &Agent, name: &str, input: Value) -> Result<Value, ToolError> {
    let tools = person::tools();
    let tool = tools.iter().find(|tool| tool.name == name).unwrap();
    tool.run(agent.session.clone(), input).await
}

fn refusal(answer: Result<Value, ToolError>) -> String {
    match answer {
        Err(ToolError::Refused(message)) => message,
        other => panic!("not a refusal: {other:?}"),
    }
}

#[tokio::test]
async fn a_new_person_names_organizations_by_id_or_by_a_name_that_fits_one() {
    let agent = agent().await;
    let ada = |organizations: Value| json!({ "first_name": "Ada", "last_name": "Lovelace", "organizations": organizations });

    let by_name = call(&agent, "people_add", ada(json!(["acme"])))
        .await
        .unwrap();
    let by_id = call(&agent, "people_add", ada(json!([agent.acme_milano])))
        .await
        .unwrap();

    assert_eq!(by_name["organizations"], json!(["Acme"]));
    assert_eq!(by_id["organizations"], json!(["Acme › Milano"]));
    let ambiguous = refusal(call(&agent, "people_add", ada(json!(["Milano"]))).await);
    assert!(
        ambiguous.contains("Acme › Milano") && ambiguous.contains("Beta › Milano"),
        "{ambiguous}"
    );
    let unknown = refusal(call(&agent, "people_add", ada(json!(["Nowhere"]))).await);
    assert!(unknown.contains("org_search"), "{unknown}");
    let found = call(
        &agent,
        "people_search",
        json!({ "text": "LOVE", "organization_id": agent.acme }),
    )
    .await
    .unwrap();
    let ids: Vec<&Value> = found["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|person| &person["id"])
        .collect();
    assert_eq!(ids, [&by_name["id"]]);
}

#[tokio::test]
async fn an_update_clears_fields_and_checks_the_version_only_when_given() {
    let agent = agent().await;
    let added = call(
        &agent,
        "people_add",
        json!({ "first_name": "Ada", "last_name": "Lovelace", "phone": "06 123",
                "organizations": ["Acme"] }),
    )
    .await
    .unwrap();
    let ciso = json!({ "person_id": added["id"], "job_title": "CISO", "clear": ["phone"],
                       "expected_version": added["version"] });

    let updated = call(&agent, "people_update", ciso.clone()).await.unwrap();

    assert_eq!(updated["job_title"], "CISO");
    let found = call(
        &agent,
        "people_search",
        json!({ "text": "ada", "response_format": "detailed" }),
    )
    .await
    .unwrap();
    let phones: Vec<Option<&Value>> = found["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|person| person.get("phone"))
        .collect();
    assert_eq!(phones, [Some(&Value::Null)]);
    let stale = refusal(call(&agent, "people_update", ciso).await);
    assert!(stale.contains("changed since"), "{stale}");
    let unchecked = json!({ "person_id": added["id"], "job_title": "CTO" });
    let updated = call(&agent, "people_update", unchecked).await.unwrap();
    assert_eq!(updated["job_title"], "CTO");
}
