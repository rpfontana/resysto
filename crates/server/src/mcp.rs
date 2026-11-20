use std::sync::Arc;

use axum::http::request::Parts;
use resysto_auth::{Agent, AgentTenant, Policies, PublicUrl, Verb, record_refusal};
use resysto_http::AppError;
use resysto_mcp::{Effect, Tool, ToolError};
use resysto_surrealdb::{Db, Grant};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool as McpTool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};

use crate::scope;

const INSTRUCTIONS: &str = "Tools for resysto, a governance, risk and compliance platform where a \
person can belong to several tenants, such as the companies an MSSP serves. Every tool but \
tenants_list acts in one tenant, which its tenant parameter names; tenants_list gives the ones you \
can act in. Within a tenant every record belongs to organizations in a tree; org_search finds them \
and the ids other tools take. You act with the permissions of the person who connected you: a tool \
their roles don't allow answers with a refusal.";

const TENANTS_LIST: &str = "tenants_list";
const TENANT: &str = "tenant";

/// A tool, with the REST resource the policies decide it on, such as `people`
#[derive(Debug, Clone)]
pub(crate) struct Mounted {
    pub(crate) resource: &'static str,
    pub(crate) tool: Tool,
}

#[derive(Clone)]
pub(crate) struct Mcp {
    db: Db,
    policies: Arc<Policies>,
    tools: Arc<[Mounted]>,
}

impl Mcp {
    pub(crate) fn new(db: Db, policies: Arc<Policies>, tools: Vec<Mounted>) -> Self {
        let tools: Vec<Mounted> = tools
            .into_iter()
            .map(|mut mounted| {
                mounted.tool.input_schema = Arc::new(with_tenant(&mounted.tool.input_schema));
                mounted
            })
            .collect();
        Self {
            db,
            policies,
            tools: tools.into(),
        }
    }

    pub(crate) fn service(
        self,
        public: &PublicUrl,
    ) -> StreamableHttpService<Self, NeverSessionManager> {
        let authority = public
            .0
            .split_once("://")
            .map_or(&*public.0, |(_, rest)| rest)
            .to_owned();
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_allowed_hosts([authority]);
        StreamableHttpService::new(
            move || Ok(self.clone()),
            Arc::new(NeverSessionManager::default()),
            config,
        )
    }

    fn allowed_somewhere(&self, mounted: &Mounted, tenants: &[AgentTenant]) -> bool {
        let verb = verb(mounted.tool.effect);
        tenants
            .iter()
            .any(|tenant| tenant.roles.allows(&self.policies, verb, mounted.resource))
    }

    async fn call(
        &self,
        agent: &Agent,
        name: &str,
        mut arguments: Map<String, Value>,
    ) -> Result<Value, ToolError> {
        if name == TENANTS_LIST {
            return list_tenants(agent).await;
        }
        let Some(mounted) = self.tools.iter().find(|mounted| mounted.tool.name == name) else {
            return Err(ToolError::refused(format!("no tool is called {name}")));
        };
        let slug = match arguments.remove(TENANT) {
            Some(Value::String(slug)) => slug,
            Some(_) => {
                return Err(ToolError::refused(
                    "tenant must be the slug of a tenant, as tenants_list gives it",
                ));
            }
            None => only_tenant(agent).await?,
        };
        let (member, roles) = agent.enter(&slug).await.map_err(|error| match error {
            AppError::NotFound => ToolError::refused(format!(
                "you can't act in the tenant {slug}; tenants_list gives the ones you can"
            )),
            AppError::MfaRequired => ToolError::refused(format!(
                "the tenant {slug} asks for a second factor this connection didn't prove; tell \
                 the person you act for to connect you again, signed in with their \
                 authenticator app"
            )),
            other => ToolError::Internal(other.into()),
        })?;
        let effect = mounted.tool.effect;
        let verb = verb(effect);
        if !roles.allows(&self.policies, verb, mounted.resource) {
            tracing::info!(
                tool = name,
                tenant = slug,
                "tool call refused by the policies"
            );
            let recorded = record_refusal(
                &self.db,
                &member.database,
                &member.actor,
                verb,
                mounted.resource,
            )
            .await;
            if let Err(error) = recorded {
                tracing::error!(%error, "refusal not recorded");
            }
            return Err(ToolError::refused(format!(
                "your roles in the tenant {slug} don't allow {name}; don't try it again there in \
                 this conversation, and tell the person you act for"
            )));
        }
        let access = scope::widen(&self.db, member)
            .await
            .map_err(|error| ToolError::Internal(error.into()))?;
        let grant = Grant {
            organizations: access.grant.organizations,
            write: effect != Effect::Read,
        };
        let session = self
            .db
            .record_session(&access.database, &access.actor, &grant)
            .await?;
        mounted.tool.run(session, Value::Object(arguments)).await
    }
}

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let Some(agent) = agent_of(&context) else {
            return Ok(ListToolsResult::with_all_items(Vec::new()));
        };
        let tenants = agent.tenants().await.map_err(|error| {
            tracing::error!(%error, "tenants of an agent not read");
            ErrorData::internal_error("the server could not read your tenants", None)
        })?;
        let mut tools = vec![tenants_list_tool()];
        tools.extend(
            self.tools
                .iter()
                .filter(|mounted| self.allowed_somewhere(mounted, &tenants))
                .map(|mounted| describe(&mounted.tool)),
        );
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(agent) = agent_of(&context) else {
            return Err(ErrorData::internal_error(
                "the request reached MCP unauthenticated",
                None,
            ));
        };
        let name = request.name.as_ref();
        if name != TENANTS_LIST && !self.tools.iter().any(|mounted| mounted.tool.name == name) {
            return Err(ErrorData::invalid_params(
                format!("no tool is called {name}"),
                None,
            ));
        }
        let arguments = request.arguments.clone().unwrap_or_default();
        let result = match self.call(agent, name, arguments).await {
            Ok(answer) => CallToolResult::structured(answer),
            Err(ToolError::Refused(message)) => text_error(message),
            Err(ToolError::Internal(error)) => {
                tracing::error!(error = ?error, tool = name, "tool failed");
                text_error("the tool failed on the server's side; try again later".to_owned())
            }
        };
        Ok(result.into())
    }
}

fn agent_of(context: &RequestContext<RoleServer>) -> Option<&Agent> {
    context.extensions.get::<Parts>()?.extensions.get::<Agent>()
}

fn describe(tool: &Tool) -> McpTool {
    let annotations = ToolAnnotations::new()
        .read_only(tool.effect == Effect::Read)
        .destructive(tool.effect == Effect::Delete);
    McpTool::new(tool.name, tool.description, Arc::clone(&tool.input_schema)).annotate(annotations)
}

fn tenants_list_tool() -> McpTool {
    let input = match json!({ "type": "object", "properties": {} }) {
        Value::Object(input) => input,
        _ => Map::new(),
    };
    McpTool::new(
        TENANTS_LIST,
        "Lists the tenants you can act in for the person who connected you, with their roles in \
         each. Every other tool takes one of these slugs as its tenant parameter, which can be \
         left out only when this list holds a single tenant.",
        Arc::new(input),
    )
    .annotate(ToolAnnotations::new().read_only(true))
}

fn with_tenant(schema: &Map<String, Value>) -> Map<String, Value> {
    let mut schema = schema.clone();
    let tenant = json!({
        "type": "string",
        "description": "The slug of the tenant to act in, from tenants_list; leave it out only \
                        when you can act in a single tenant",
    });
    match schema.get_mut("properties") {
        Some(Value::Object(properties)) => {
            properties.insert(TENANT.to_owned(), tenant);
        }
        _ => {
            schema.insert("properties".to_owned(), json!({ TENANT: tenant }));
        }
    }
    schema
}

async fn list_tenants(agent: &Agent) -> Result<Value, ToolError> {
    let tenants = agent
        .tenants()
        .await
        .map_err(|error| ToolError::Internal(error.into()))?;
    let tenants: Vec<Value> = tenants
        .iter()
        .map(|tenant| json!({ "slug": tenant.slug, "roles": tenant.roles.names() }))
        .collect();
    Ok(json!({ "tenants": tenants }))
}

async fn only_tenant(agent: &Agent) -> Result<String, ToolError> {
    let tenants = agent
        .tenants()
        .await
        .map_err(|error| ToolError::Internal(error.into()))?;
    match <[AgentTenant; 1]>::try_from(tenants) {
        Ok([only]) => Ok(only.slug),
        Err(none) if none.is_empty() => Err(ToolError::refused(
            "you can't act in any tenant now; tell the person you act for",
        )),
        Err(several) => {
            let slugs: Vec<&str> = several.iter().map(|tenant| tenant.slug.as_str()).collect();
            Err(ToolError::refused(format!(
                "say which tenant to act in with the tenant parameter: one of {}",
                slugs.join(", ")
            )))
        }
    }
}

fn verb(effect: Effect) -> Verb {
    match effect {
        Effect::Read => Verb::Read,
        Effect::Write => Verb::Write,
        Effect::Delete => Verb::Delete,
    }
}

fn text_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, header};
    use resysto_auth::{
        Cipher, MasterName, Passwords, add_member, create_account, create_grant, issue_access_token,
    };
    use resysto_model::{Email, OrganizationId};
    use resysto_surrealdb::Db;
    use schemars::JsonSchema;
    use secrecy::SecretString;
    use serde::Deserialize;
    use surrealdb::Surreal;
    use surrealdb::engine::any::Any;
    use surrealdb::types::{RecordId, Uuid};
    use tower::ServiceExt;

    use super::*;
    use crate::routes::mcp_routes;
    use crate::state::AppState;

    const CLAUDE: &str = "https://claude.ai/oauth/claude-code-client-metadata";

    #[derive(Deserialize)]
    struct Config {
        db: DbConfig,
        auth: AuthConfig,
    }

    #[derive(Deserialize)]
    struct DbConfig {
        endpoint: String,
        namespace: String,
        migrator_user: String,
        migrator_password: SecretString,
    }

    #[derive(Deserialize)]
    struct AuthConfig {
        signing_key: SecretString,
        verifying_key: String,
    }

    #[derive(Deserialize, JsonSchema)]
    struct Nothing {}

    struct Stack {
        app: Router,
        tenant: Surreal<Any>,
        reader: String,
        admin: String,
    }

    // beta needs no database; the extra tool claims to read but tries to write
    async fn stack() -> Stack {
        let Config { db, auth } = resysto_config::load().unwrap();
        let server = Db::connect(resysto_surrealdb::Config {
            endpoint: db.endpoint,
            namespace: db.namespace,
            username: db.migrator_user,
            password: db.migrator_password,
            signing_key: auth.signing_key,
            verifying_key: auth.verifying_key,
        })
        .await
        .unwrap();
        let id = Uuid::new_v4().simple();
        let (master_name, tenant_name) = (format!("t_test_{id}_master"), format!("t_test_{id}"));
        resysto_migrator::provision_master(&server, &master_name)
            .await
            .unwrap();
        resysto_migrator::provision_tenant(&server, &tenant_name)
            .await
            .unwrap();
        let master = server.system_session(&master_name).await.unwrap();
        let mut created = master
            .query(
                "CREATE ONLY tenant SET slug = 'acme', db_name = $name, state = 'ready'
                     RETURN VALUE id;
                 CREATE ONLY tenant SET slug = 'beta', db_name = 't_beta', state = 'ready'
                     RETURN VALUE id;",
            )
            .bind(("name", tenant_name.clone()))
            .await
            .unwrap();
        let acme_tenant: Option<RecordId> = created.take(0).unwrap();
        let beta_tenant: Option<RecordId> = created.take(1).unwrap();
        let (acme_tenant, beta_tenant) = (acme_tenant.unwrap(), beta_tenant.unwrap());
        let tenant = server.system_session(&tenant_name).await.unwrap();
        let acme = OrganizationId::new();
        tenant
            .query("CREATE $acme SET name = 'Acme', path = []")
            .bind(("acme", acme))
            .await
            .unwrap()
            .check()
            .unwrap();
        let passwords = Passwords::new(SecretString::from("pepper")).unwrap();
        let mut tokens = Vec::new();
        for (email, role, beta) in [
            ("ada@acme.it", "tenant/reader", false),
            ("bob@acme.it", "tenant/admin", true),
        ] {
            let email: Email = email.parse().unwrap();
            let password = "a long enough password".to_owned();
            let account = create_account(&master, &passwords, email, password)
                .await
                .unwrap();
            add_member(&server, &master, account, "acme", &[role.to_owned()])
                .await
                .unwrap();
            let mut tenants = vec![acme_tenant.clone()];
            if beta {
                master
                    .query("RELATE $account->member_of->$beta SET roles = [$role]")
                    .bind(("account", account))
                    .bind(("beta", beta_tenant.clone()))
                    .bind(("role", role))
                    .await
                    .unwrap()
                    .check()
                    .unwrap();
                tenants.push(beta_tenant.clone());
            }
            let grant = create_grant(&master, account, tenants, CLAUDE, "Claude Code", true, None)
                .await
                .unwrap();
            let token = issue_access_token(&master, grant).await.unwrap();
            tokens.push(token);
        }
        let [reader, admin] = <[String; 2]>::try_from(tokens).unwrap();
        let sneak = Tool::new(
            "people_sneak",
            Effect::Read,
            "claims to read, tries to write",
            move |db: Surreal<Any>, _: Nothing| async move {
                let created: Vec<Value> = db
                    .query(
                        "CREATE person SET first_name = 'Eve', last_name = 'X',
                             organization_ids = [$acme]",
                    )
                    .bind(("acme", acme))
                    .await
                    .map_err(|error| ToolError::Internal(error.into()))?
                    .take(0)
                    .map_err(|error| ToolError::Internal(error.into()))?;
                Ok::<_, ToolError>(json!({ "created": !created.is_empty() }))
            },
        );
        let mut tools: Vec<Mounted> = resysto_people::person::tools()
            .into_iter()
            .map(|tool| Mounted {
                resource: "people",
                tool,
            })
            .collect();
        tools.push(Mounted {
            resource: "people",
            tool: sneak,
        });
        let state = AppState {
            db: server,
            master: MasterName(master_name.into()),
            passwords,
            cipher: Cipher::new(&SecretString::from("7".repeat(64))).unwrap(),
            policies: Arc::new(Policies::load().unwrap()),
            public: PublicUrl("http://localhost:3000".into()),
        };
        let app = Router::new()
            .nest("/mcp", mcp_routes(&state, tools))
            .with_state(state);
        Stack {
            app,
            tenant,
            reader,
            admin,
        }
    }

    async fn rpc(app: &Router, token: &str, method: &str, params: Value) -> Value {
        let name = params["name"].as_str().map(ToOwned::to_owned);
        let mut params = params;
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let mut request = Request::post("/mcp")
            .header(header::HOST, "localhost:3000")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", method);
        if let Some(name) = name {
            request = request.header("mcp-name", name);
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(status.is_success(), "{method}: {status} {text}");
        serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("{method}: {error} {text}"))
    }

    // the 2025-11-25 revision, which claude.ai uses
    async fn legacy_rpc(app: &Router, token: &str, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let request = Request::post("/mcp")
            .header(header::HOST, "localhost:3000")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(status.is_success(), "{method}: {status} {text}");
        serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("{method}: {error} {text}"))
    }

    async fn call(
        app: &Router,
        token: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let params = json!({ "name": name, "arguments": arguments });
        let answer = rpc(app, token, "tools/call", params).await;
        let result = &answer["result"];
        if result["isError"] == json!(true) {
            Err(result["content"][0]["text"].as_str().unwrap().to_owned())
        } else {
            Ok(result["structuredContent"].clone())
        }
    }

    #[tokio::test]
    async fn a_client_of_the_previous_revision_initializes_then_lists_without_a_session() {
        let Stack { app, reader, .. } = stack().await;
        let hello = json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "claude-ai", "version": "1" },
        });

        let initialized = legacy_rpc(&app, &reader, "initialize", hello).await;
        let listed = legacy_rpc(&app, &reader, "tools/list", json!({})).await;

        assert_eq!(
            initialized["result"]["protocolVersion"],
            json!("2025-11-25"),
            "{initialized}"
        );
        let count = listed["result"]["tools"].as_array().map(Vec::len);
        assert_eq!(count, Some(3), "{listed}");
    }

    #[tokio::test]
    async fn an_agent_acts_in_the_tenant_each_call_names_with_what_its_roles_allow_there() {
        let Stack {
            app,
            tenant,
            reader,
            admin,
        } = stack().await;

        let listed = rpc(&app, &reader, "tools/list", json!({})).await;
        let tools = listed["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["tenants_list", "people_search", "people_sneak"]);
        assert!(
            tools[1]["inputSchema"]["properties"]["tenant"].is_object(),
            "{listed}"
        );
        let tenants = call(&app, &admin, "tenants_list", json!({})).await;
        let both = json!({ "tenants": [
            { "slug": "acme", "roles": ["tenant/admin"] },
            { "slug": "beta", "roles": ["tenant/admin"] },
        ] });
        assert_eq!(tenants, Ok(both));

        let ada =
            json!({ "first_name": "Ada", "last_name": "Lovelace", "organizations": ["Acme"] });
        let unnamed = call(&app, &admin, "people_add", ada.clone()).await;
        assert!(
            unnamed
                .as_ref()
                .is_err_and(|refusal| refusal.ends_with("one of acme, beta")),
            "{unnamed:?}"
        );
        let foreign = json!({ "text": "Ada", "tenant": "beta" });
        let foreign = call(&app, &reader, "people_search", foreign).await;
        assert!(
            foreign
                .as_ref()
                .is_err_and(|refusal| refusal.starts_with("you can't act in the tenant beta")),
            "{foreign:?}"
        );
        // the reader's only tenant needs no naming
        let refused = call(&app, &reader, "people_add", ada.clone()).await;
        assert!(refused.is_err(), "{refused:?}");
        let mut in_acme = ada;
        in_acme["tenant"] = json!("acme");
        let added = call(&app, &admin, "people_add", in_acme).await;
        assert!(added.is_ok(), "{added:?}");
        let sneak = json!({ "tenant": "acme" });
        let sneaked = call(&app, &admin, "people_sneak", sneak).await;
        assert_eq!(sneaked, Ok(json!({ "created": false })));

        let mut trail = tenant
            .query(
                "SELECT event, via, client, resource FROM audit_event;
                 SELECT VALUE [event, via, client, after.first_name] FROM audit_change
                     WHERE record::tb(record) = 'person';",
            )
            .await
            .unwrap();
        let refusals: Vec<Value> = trail.take(0).unwrap();
        let changes: Vec<Value> = trail.take(1).unwrap();
        let refusal =
            json!({ "event": "refused", "via": "mcp", "client": CLAUDE, "resource": "people" });
        assert_eq!(refusals, [refusal]);
        assert_eq!(changes, [json!(["CREATE", "mcp", CLAUDE, "Ada"])]);
    }
}
