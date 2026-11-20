use std::fmt::Write;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use resysto_http::{AppError, AppResult};
use resysto_model::OrganizationId;
use resysto_surrealdb::{Actor, Db, Via};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};

use super::account::AccountId;
use super::audit::Client;
use super::error::AuthError;
use super::http::{MasterName, REQUEST_ID, TenantMember, master};
use super::membership::needs_mfa;
use super::mfa::MfaRequiredFor;
use super::policy::{Member, TenantRoles};
use super::token::key;

/// The URL clients reach the server at; set by the deployment, never read from a request's `Host`
#[derive(Debug, Clone)]
pub struct PublicUrl(pub Arc<str>);

/// An agent with a live access token, put in the request extensions by [`require_agent`]
#[derive(Debug, Clone)]
pub struct Agent {
    master: Surreal<Any>,
    account: AccountId,
    client: String,
    mfa: bool,
    tenants: Vec<RecordId>,
    session: String,
    request: Option<String>,
    from: Client,
}

/// A tenant an agent can act in, with the roles of its member there
#[derive(Debug, Clone)]
pub struct AgentTenant {
    pub slug: String,
    pub roles: TenantRoles,
}

#[derive(SurrealValue)]
struct Bearer {
    account: AccountId,
    client: String,
    mfa: bool,
    tenants: Vec<RecordId>,
}

#[derive(SurrealValue)]
struct Reached {
    slug: String,
    database: String,
    organizations: Option<Vec<OrganizationId>>,
    roles: Vec<String>,
    mfa_required_for: MfaRequiredFor,
}

/// Lets a request through to the MCP endpoint only with a live access token
pub async fn require_agent(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(public): State<PublicUrl>,
    client: Client,
    mut request: Request,
    next: Next,
) -> AppResult<Response> {
    let Some(token) = bearer(request.headers()) else {
        return Ok(challenge(&public, None));
    };
    let master = master(&db, &name).await?;
    let token_key = key(token);
    let Some(found) = granted(&master, &token_key).await? else {
        tracing::info!("MCP request refused: unknown or expired access token");
        return Ok(challenge(&public, Some("invalid_token")));
    };
    let agent = Agent {
        master,
        account: found.account,
        client: found.client,
        mfa: found.mfa,
        tenants: found.tenants,
        session: token_key,
        request: request
            .headers()
            .get(REQUEST_ID)
            .and_then(|id| id.to_str().ok())
            .map(ToOwned::to_owned),
        from: client,
    };
    request.extensions_mut().insert(agent);
    Ok(next.run(request).await)
}

impl Agent {
    /// The tenants of the grant the agent can still enter
    pub async fn tenants(&self) -> AppResult<Vec<AgentTenant>> {
        let reached = self.reach(None).await?;
        Ok(reached
            .into_iter()
            .filter(|reached| !self.unproved(reached))
            .map(|reached| AgentTenant {
                roles: TenantRoles(Member {
                    account: self.account,
                    whole_tenant: reached.organizations.is_none(),
                    roles: reached.roles,
                    tenant: reached.database,
                }),
                slug: reached.slug,
            })
            .collect())
    }

    /// The member the agent acts as in the tenant `slug`, with its roles there
    pub async fn enter(&self, slug: &str) -> AppResult<(TenantMember, TenantRoles)> {
        let reached = self
            .reach(Some(slug))
            .await?
            .into_iter()
            .next()
            .ok_or(AppError::NotFound)?;
        if self.unproved(&reached) {
            return Err(AppError::MfaRequired);
        }
        let roles = TenantRoles(Member {
            account: self.account,
            whole_tenant: reached.organizations.is_none(),
            roles: reached.roles,
            tenant: reached.database.clone(),
        });
        let actor = Actor {
            member: self.account.cast::<resysto_model::Member>().into(),
            via: Via::Mcp,
            client: Some(self.client.clone()),
            session: Some(self.session.clone()),
            request: self.request.clone(),
            ip: self.from.ip,
            user_agent: self.from.user_agent.clone(),
        };
        let member = TenantMember {
            database: reached.database,
            actor,
            organizations: reached.organizations,
            write: false,
        };
        Ok((member, roles))
    }

    fn unproved(&self, reached: &Reached) -> bool {
        !self.mfa && needs_mfa(reached.mfa_required_for, &reached.roles)
    }

    async fn reach(&self, slug: Option<&str>) -> Result<Vec<Reached>, AuthError> {
        let reached: Vec<Reached> = self
            .master
            .query(
                "SELECT out.slug AS slug, out.db_name AS database, org_scope AS organizations,
                     roles, out.mfa_required_for AS mfa_required_for
                 FROM member_of
                 WHERE in = $account AND out IN $tenants AND out.state = 'ready'
                     AND ($slug = NONE OR out.slug = $slug)
                 ORDER BY slug",
            )
            .bind(("account", self.account))
            .bind(("tenants", self.tenants.clone()))
            .bind(("slug", slug.map(ToOwned::to_owned)))
            .await?
            .check()?
            .take(0)?;
        Ok(reached)
    }
}

async fn granted(master: &Surreal<Any>, token_key: &str) -> Result<Option<Bearer>, AuthError> {
    let found: Option<Bearer> = master
        .query(
            "SELECT grant.account AS account, grant.client AS client, grant.mfa AS mfa,
                 grant.tenants AS tenants
             FROM type::record('access_token', $key)
             WHERE expires_at > time::now() AND grant.expires_at > time::now()
                 AND grant.account.active = true",
        )
        .bind(("key", token_key.to_owned()))
        .await?
        .check()?
        .take(0)?;
    Ok(found)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

// RFC 9728
fn challenge(public: &PublicUrl, error: Option<&str>) -> Response {
    let mut value = format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
        public.0
    );
    if let Some(error) = error {
        let _ = write!(value, ", error=\"{error}\"");
    }
    let mut response = AppError::Unauthorized.into_response();
    if let Ok(value) = HeaderValue::try_from(value) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::extract::FromRef;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::{Extension, Router, middleware};
    use tower::ServiceExt;

    use super::*;
    use crate::account::create_account;
    use crate::membership::add_member;
    use crate::testing::{PASSWORD, access_token, ada, new_master, new_tenant, passwords, server};

    const CLAUDE: &str = "https://claude.ai/oauth/claude-code-client-metadata";

    #[derive(Clone)]
    struct TestState {
        db: Db,
        master: MasterName,
        public: PublicUrl,
    }

    impl FromRef<TestState> for Db {
        fn from_ref(state: &TestState) -> Self {
            state.db.clone()
        }
    }

    impl FromRef<TestState> for MasterName {
        fn from_ref(state: &TestState) -> Self {
            state.master.clone()
        }
    }

    impl FromRef<TestState> for PublicUrl {
        fn from_ref(state: &TestState) -> Self {
            state.public.clone()
        }
    }

    async fn reach(Extension(agent): Extension<Agent>) -> String {
        let tenants: Vec<String> = agent
            .tenants()
            .await
            .unwrap()
            .into_iter()
            .map(|tenant| tenant.slug)
            .collect();
        let mut seen = vec![tenants.join(",")];
        for slug in ["acme", "beta", "gamma", "nobody"] {
            seen.push(match agent.enter(slug).await {
                Ok((member, _)) => format!(
                    "{slug}: {:?} {:?} {}",
                    member.actor.via, member.actor.client, member.write
                ),
                Err(error) => format!("{slug}: {error:?}"),
            });
        }
        seen.join("\n")
    }

    #[tokio::test]
    async fn an_agent_acts_only_in_the_tenants_of_its_grant_that_still_let_its_member_in() {
        let server = server().await;
        let (name, master) = new_master(&server).await;
        new_tenant(&server, &master, "acme").await;
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();
        let reader = vec!["tenant/reader".to_owned()];
        add_member(&server, &master, account, "acme", &reader)
            .await
            .unwrap();
        // beta and gamma need no database: entering one only reads the master
        master
            .query(
                "LET $beta = CREATE ONLY tenant SET slug = 'beta', db_name = 't_beta',
                     state = 'ready';
                 LET $gamma = CREATE ONLY tenant SET slug = 'gamma', db_name = 't_gamma',
                     state = 'ready', mfa_required_for = 'everyone';
                 RELATE $account->member_of->($beta.id) SET roles = $roles;
                 RELATE $account->member_of->($gamma.id) SET roles = $roles;",
            )
            .bind(("account", account))
            .bind(("roles", reader))
            .await
            .unwrap()
            .check()
            .unwrap();
        let live = access_token(&master, account, &["acme", "gamma"], CLAUDE).await;
        let expired = access_token(&master, account, &["acme"], CLAUDE).await;
        master
            .query("UPDATE type::record('access_token', $key) SET expires_at = time::now() - 1s")
            .bind(("key", key(&expired)))
            .await
            .unwrap()
            .check()
            .unwrap();
        let state = TestState {
            db: server.clone(),
            master: MasterName(name.into()),
            public: PublicUrl("https://resysto.example".into()),
        };
        let app = Router::new()
            .route("/mcp", get(reach))
            .route_layer(middleware::from_fn_with_state(state.clone(), require_agent))
            .with_state(state);
        let call = async |token: Option<&str>| {
            let mut request = Request::get("/mcp");
            if let Some(token) = token {
                request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            app.clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
        };

        let metadata = "https://resysto.example/.well-known/oauth-protected-resource/mcp";
        let cases = [
            (None, format!("Bearer resource_metadata=\"{metadata}\"")),
            (
                Some(expired.as_str()),
                format!("Bearer resource_metadata=\"{metadata}\", error=\"invalid_token\""),
            ),
            (
                Some("forged"),
                format!("Bearer resource_metadata=\"{metadata}\", error=\"invalid_token\""),
            ),
        ];
        for (token, challenge) in cases {
            let refused = call(token).await;
            assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{token:?}");
            assert_eq!(
                refused.headers()[header::WWW_AUTHENTICATE],
                challenge.as_str(),
                "{token:?}"
            );
        }
        let allowed = call(Some(&live)).await;
        assert_eq!(allowed.status(), StatusCode::OK);
        let body = to_bytes(allowed.into_body(), usize::MAX).await.unwrap();
        let expected = format!(
            "acme\nacme: Mcp Some({CLAUDE:?}) false\nbeta: NotFound\ngamma: MfaRequired\n\
             nobody: NotFound"
        );
        assert_eq!(String::from_utf8_lossy(&body), expected);
    }
}
