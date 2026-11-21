use resysto_mcp::ToolError;
use resysto_model::{MemberId, OrganizationId};
use resysto_org::organization;
use resysto_surrealdb::{Actor, Grant, Via};
use serde_json::{Value, json};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::{TENANT, tenant};

// what the tool called `name` answers, as the server's MCP endpoint calls it
async fn call(session: &Surreal<Any>, name: &str, input: Value) -> Result<Value, ToolError> {
    let tools = organization::tools();
    let tool = tools.iter().find(|tool| tool.name == name).unwrap();
    tool.run(session.clone(), input).await
}

#[tokio::test]
async fn an_overview_names_the_branches_and_counts_what_belongs_to_the_organization() {
    let db = tenant().await;
    let [acme, milano, beta, beta_milano] = [(); 4].map(|()| OrganizationId::new());
    db.system_session(TENANT)
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
    let session = db.record_session(TENANT, &actor, &reading).await.unwrap();
    let overview = |organization: Value| json!({ "organization": organization });

    let shown = call(&session, "org_overview", overview(json!("acme")))
        .await
        .unwrap();

    assert_eq!(shown["path"], "Acme");
    assert_eq!(
        shown["branches"],
        json!([{ "id": milano, "name": "Milano" }])
    );
    assert_eq!(shown["records"]["person"], 1);
    let by_id = call(&session, "org_overview", overview(json!(milano)))
        .await
        .unwrap();
    assert_eq!(by_id["path"], "Acme › Milano");
    match call(&session, "org_overview", overview(json!("Milano"))).await {
        Err(ToolError::Refused(message)) => assert!(
            message.contains("Acme › Milano") && message.contains("Beta › Milano"),
            "{message}"
        ),
        other => panic!("an ambiguous name went through: {other:?}"),
    }
}
