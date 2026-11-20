use std::net::IpAddr;

use axum::extract::{FromRef, State};
use axum::{Extension, Json};
use resysto_http::{AppError, AppResult, ErrorResponse, Query, TenantAccess};
use resysto_id::{Entity, Id};
use resysto_model::MemberId;
use resysto_surrealdb::{Actor, Cursor, Db, Executor, Page, PageRequest, page};
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::account::AccountId;
use super::error::AuthError;
use crate::policy::Verb;

/// What happened to an account, a session or a tenant, as the master records it
#[derive(Debug, Clone, Copy, PartialEq, Eq, SurrealValue)]
#[surreal(untagged, rename_all = "snake_case")]
pub(crate) enum Event {
    AccountCreated,
    MemberAdded,
    AccessTokenIssued,
    AppAllowed,
    AppRefused,
    RefreshTokenReused,
    GrantRevoked,
    Login,
    LoginFailed,
    SecondFactorRefused,
    RecoveryCodeUsed,
    Logout,
    PasswordChanged,
    PasswordRefused,
    SessionEnded,
    OtherSessionsEnded,
    TotpBound,
    TotpUnbound,
    TotpReset,
    MfaRequirementChanged,
    Unlocked,
}

/// Where a request comes from; empty for what runs outside a request, such as an unlock
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Client {
    /// As the server's `ClientIpSource` reads it
    pub(crate) ip: Option<IpAddr>,
    pub(crate) user_agent: Option<String>,
}

/// What an event names besides itself
#[derive(Debug, Default)]
pub(crate) struct About {
    pub(crate) account: Option<AccountId>,
    /// The key of the session the event happened in, or ended
    pub(crate) session: Option<String>,
    pub(crate) tenant: Option<RecordId>,
    pub(crate) app: Option<String>,
    pub(crate) tenants: Option<Vec<RecordId>>,
    pub(crate) client: Client,
}

impl About {
    pub(crate) fn of(account: AccountId) -> Self {
        Self {
            account: Some(account),
            ..Self::default()
        }
    }

    /// `key` is the session's, never its token
    pub(crate) fn in_session(self, key: String) -> Self {
        Self {
            session: Some(key),
            ..self
        }
    }

    pub(crate) fn with_client(self, client: &Client) -> Self {
        Self {
            client: client.clone(),
            ..self
        }
    }
}

enum ChangeRow {}

impl Entity for ChangeRow {
    const TABLE: &'static str = "audit_change";
}

type ChangeId = Id<ChangeRow>;

enum EventRow {}

impl Entity for EventRow {
    const TABLE: &'static str = "audit_event";
}

type EventId = Id<EventRow>;

/// A record a write created, updated or deleted, with who wrote and how
#[derive(Debug, Serialize, SurrealValue, ToSchema)]
struct Change {
    id: ChangeId,
    #[schema(value_type = String, format = DateTime)]
    at: Datetime,
    event: String,
    table: String,
    record: serde_json::Value,
    /// `None` when the system wrote
    by: Option<MemberId>,
    by_email: Option<String>,
    via: Option<String>,
    client: Option<String>,
    session: Option<String>,
    request: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
}

/// What happened in the tenant without changing a record
#[derive(Debug, Serialize, SurrealValue, ToSchema)]
struct TenantEvent {
    id: EventId,
    #[schema(value_type = String, format = DateTime)]
    at: Datetime,
    event: String,
    by: Option<MemberId>,
    by_email: Option<String>,
    via: Option<String>,
    client: Option<String>,
    session: Option<String>,
    request: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
    action: Option<String>,
    resource: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ChangesQuery {
    /// The `next_cursor` of the previous page; absent on the first one
    #[param(value_type = Option<String>, format = Uuid)]
    after: Option<Cursor>,
    /// Rows wanted: 20 when absent, at least 1 and at most 200
    limit: Option<u32>,
    /// `<table>:<uuid>`, such as `person:0190…`, for the history of one record
    record: Option<String>,
}

const CHANGES: &str =
    "SELECT id, at, event, record::tb(record) AS table, record::id(record) AS record,
         by, by.email AS by_email, via, client, session, request, ip, user_agent, before, after
     FROM audit_change WHERE id < type::record('audit_change', $before)
     ORDER BY id DESC LIMIT $limit";

const RECORD_CHANGES: &str = "SELECT id, at, event, record::tb(record) AS table,
         record::id(record) AS record, by, by.email AS by_email, via, client, session, request, ip,
         user_agent, before, after
     FROM audit_change WHERE record = $record AND id < type::record('audit_change', $before)
     ORDER BY id DESC LIMIT $limit";

const EVENTS: &str =
    "SELECT id, at, event, by, by.email AS by_email, via, client, session, request, ip, user_agent,
         action, resource
     FROM audit_event WHERE id < type::record('audit_event', $before)
     ORDER BY id DESC LIMIT $limit";

/// The routes of `/t/{slug}/api/v1/audit`
pub fn audit_router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Db: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(changes))
        .routes(routes!(events))
}

pub(crate) async fn record(
    master: &impl Executor,
    event: Event,
    about: About,
) -> Result<(), resysto_surrealdb::Error> {
    master
        .query(
            "CREATE type::record('audit_event', rand::uuid::v7())
             SET event = $kind, account = $account, session = $session_key, tenant = $tenant,
                 client = $app, tenants = $tenants, ip = $ip, user_agent = $user_agent
             RETURN NONE",
        )
        .bind(("kind", event))
        .bind(("account", about.account))
        .bind(("session_key", about.session))
        .bind(("tenant", about.tenant))
        .bind(("app", about.app))
        .bind(("tenants", about.tenants))
        .bind(("ip", about.client.ip.map(|ip| ip.to_string())))
        .bind(("user_agent", about.client.user_agent))
        .await?
        .check()?;
    Ok(())
}

/// Records in the tenant's database that the policies refused `actor` a request
pub async fn record_refusal(
    db: &Db,
    database: &str,
    actor: &Actor,
    verb: Verb,
    resource: &str,
) -> Result<(), AuthError> {
    db.system_session(database)
        .await?
        .query(
            "CREATE type::record('audit_event', rand::uuid::v7())
             SET event = 'refused', by = $by, via = $via, client = $client, session = $session_key,
                 request = $request,
                 ip = $ip, user_agent = $user_agent, action = $action, resource = $resource
             RETURN NONE",
        )
        .bind(("by", actor.member.clone()))
        .bind(("via", actor.via))
        .bind(("client", actor.client.clone()))
        .bind(("session_key", actor.session.clone()))
        .bind(("request", actor.request.clone()))
        .bind(("ip", actor.ip.map(|ip| ip.to_string())))
        .bind(("user_agent", actor.user_agent.clone()))
        .bind(("action", verb.name()))
        .bind(("resource", resource.to_owned()))
        .await?
        .check()?;
    Ok(())
}

// the audit tables refuse every record session: the policies already kept out all but the
// admins of the whole tenant
/// List the changes
///
/// The records written in the tenant, newest first, with who wrote them and how; `record` narrows the list to one record's history
#[utoipa::path(
    get,
    operation_id = "audit_changes_list",
    path = "/changes",
    tag = "audit",
    params(ChangesQuery),
    responses(
        (status = 200, body = Page<Change>),
        (status = 400, body = ErrorResponse, description = "`record` is not `<table>:<uuid>`"),
    ),
)]
async fn changes(
    State(db): State<Db>,
    Extension(access): Extension<TenantAccess>,
    Query(query): Query<ChangesQuery>,
) -> AppResult<Json<Page<Change>>> {
    let tenant = system(&db, &access).await?;
    let request = PageRequest {
        after: query.after,
        limit: query.limit,
    };
    let found = match query.record {
        None => page(tenant.query(CHANGES), request).await,
        Some(raw) => {
            let record = parse_record(&raw)?;
            page(
                tenant.query(RECORD_CHANGES).bind(("record", record)),
                request,
            )
            .await
        }
    };
    Ok(Json(found.map_err(internal)?))
}

/// List the events
///
/// What happened in the tenant without changing a record, such as a request the policies refused
#[utoipa::path(
    get,
    operation_id = "audit_events_list",
    path = "/events",
    tag = "audit",
    params(PageRequest),
    responses((status = 200, body = Page<TenantEvent>)),
)]
async fn events(
    State(db): State<Db>,
    Extension(access): Extension<TenantAccess>,
    Query(request): Query<PageRequest>,
) -> AppResult<Json<Page<TenantEvent>>> {
    let tenant = system(&db, &access).await?;
    Ok(Json(
        page(tenant.query(EVENTS), request)
            .await
            .map_err(internal)?,
    ))
}

async fn system(db: &Db, access: &TenantAccess) -> AppResult<Surreal<Any>> {
    db.system_session(&access.database).await.map_err(internal)
}

fn internal(error: resysto_surrealdb::Error) -> AppError {
    AppError::Internal(error.into())
}

fn parse_record(raw: &str) -> AppResult<RecordId> {
    let invalid = || AppError::Validation(format!("record must be <table>:<uuid>, not {raw:?}"));
    let (table, key) = raw.split_once(':').ok_or_else(invalid)?;
    let plain = !table.is_empty() && table.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if !plain {
        return Err(invalid());
    }
    let key = uuid::Uuid::try_parse(key).map_err(|_| invalid())?;
    Ok(RecordId::new(table, surrealdb::types::Uuid::from(key)))
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use resysto_surrealdb::{Grant, Via};
    use serde_json::Value;
    use surrealdb::types::Uuid;
    use tower::ServiceExt;

    use super::*;
    use crate::account::create_account;
    use crate::login::login;
    use crate::session::logout;
    use crate::testing::{
        PASSWORD, ada, master, new_master, new_tenant, passwords, server, session_token, sign_in,
    };
    use crate::token::key;

    #[derive(Debug, PartialEq, SurrealValue)]
    struct Recorded {
        event: Event,
        account: Option<AccountId>,
        session: Option<String>,
        ip: Option<String>,
    }

    #[tokio::test]
    async fn the_master_records_what_happens_to_an_account() {
        let master = master().await;
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();

        let from_office = Client {
            ip: Some(IpAddr::from([10, 0, 0, 7])),
            user_agent: Some("Firefox".into()),
        };
        let wrong = login(&master, &passwords(), ada(), "wrong".into(), &from_office).await;
        assert!(wrong.is_err(), "a wrong password signed in");
        let token = session_token(sign_in(&master, ada()).await);
        logout(&master, &token, &from_office).await.unwrap();

        let recorded: Vec<Recorded> = master
            .query("SELECT event, account, session, ip FROM audit_event ORDER BY at")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let row = |event, session: Option<&str>, ip: Option<&str>| Recorded {
            event,
            account: Some(account),
            session: session.map(Into::into),
            ip: ip.map(Into::into),
        };
        let session = key(&token);
        // sign_in comes from no address
        let expected = [
            row(Event::AccountCreated, None, None),
            row(Event::LoginFailed, None, Some("10.0.0.7")),
            row(Event::Login, Some(&session), None),
            row(Event::Logout, Some(&session), Some("10.0.0.7")),
        ];
        assert_eq!(recorded, expected);
    }

    async fn get(server: &Db, database: &str, uri: &str) -> Value {
        let access = TenantAccess {
            database: database.into(),
            actor: Actor {
                member: MemberId::new().into(),
                via: Via::Ui,
                client: None,
                session: None,
                request: None,
                ip: None,
                user_agent: None,
            },
            grant: Grant {
                organizations: None,
                write: false,
            },
        };
        let request = Request::get(uri)
            .extension(access)
            .body(Body::empty())
            .unwrap();
        let response = axum::Router::<Db>::from(audit_router())
            .with_state(server.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn the_audit_lists_changes_newest_first_and_the_history_of_one_record() {
        let server = server().await;
        let (_, master) = new_master(&server).await;
        let tenant = new_tenant(&server, &master, "acme").await;
        let member = MemberId::new();
        let writer = Actor {
            member: member.into(),
            via: Via::Ui,
            client: None,
            session: Some("5e55".into()),
            request: Some("r-1".into()),
            ip: Some(IpAddr::from([10, 0, 0, 7])),
            user_agent: Some("Firefox".into()),
        };
        let everything = Grant {
            organizations: None,
            write: true,
        };
        server
            .system_session(&tenant)
            .await
            .unwrap()
            .query("UPSERT $member SET email = 'ada@acme.it'")
            .bind(("member", member))
            .await
            .unwrap()
            .check()
            .unwrap();
        let session = server
            .record_session(&tenant, &writer, &everything)
            .await
            .unwrap();
        let (one, two) = (Uuid::new_v7(), Uuid::new_v7());
        session
            .query(
                "CREATE type::record('organization', $one) SET name = 'one', path = [];
                 CREATE type::record('organization', $two) SET name = 'two', path = [];
                 UPDATE type::record('organization', $one) SET name = 'uno';",
            )
            .bind(("one", one))
            .bind(("two", two))
            .await
            .unwrap()
            .check()
            .unwrap();
        record_refusal(&server, &tenant, &writer, Verb::Delete, "people")
            .await
            .unwrap();

        let all = get(&server, &tenant, "/changes?limit=2").await;
        let names: Vec<&Value> = all["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|change| &change["after"]["name"])
            .collect();
        assert_eq!(names, ["uno", "two"]);
        assert!(all["next_cursor"].is_string(), "{all}");

        let uri = format!("/changes?record=organization:{one}");
        let history = get(&server, &tenant, &uri).await;
        let rows = history["data"].as_array().unwrap();
        let seen: Vec<[&str; 6]> = rows
            .iter()
            .map(|row| {
                ["event", "record", "by_email", "request", "ip", "user_agent"]
                    .map(|field| row[field].as_str().unwrap())
            })
            .collect();
        let one = one.to_string();
        let change = |event: &'static str| {
            [
                event,
                one.as_str(),
                "ada@acme.it",
                "r-1",
                "10.0.0.7",
                "Firefox",
            ]
        };
        assert_eq!(seen, [change("UPDATE"), change("CREATE")]);

        let events = get(&server, &tenant, "/events").await;
        let refusal = &events["data"][0];
        assert_eq!(
            (&refusal["event"], &refusal["action"], &refusal["resource"]),
            (
                &Value::from("refused"),
                &Value::from("delete"),
                &Value::from("people")
            )
        );
        assert_eq!(refusal["by"], Value::from(member.to_string()));

        let refused = axum::Router::<Db>::from(audit_router())
            .with_state(server.clone())
            .oneshot(
                Request::get("/changes?record=organization")
                    .extension(TenantAccess {
                        database: tenant.clone(),
                        actor: writer.clone(),
                        grant: everything.clone(),
                    })
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    }
}
