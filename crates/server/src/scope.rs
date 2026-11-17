use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use resysto_auth::TenantMember;
use resysto_http::{AppError, AppResult, TenantAccess};
use resysto_org::organization;
use resysto_surrealdb::{Db, Grant};
use surrealdb::types::RecordId;

/// Widens the organizations of the member `require_member` let through to their branches, and
/// opens the request's way to the tenant database
pub(crate) async fn widen_to_branches(
    State(db): State<Db>,
    mut request: Request,
    next: Next,
) -> AppResult<Response> {
    let Some(member) = request.extensions_mut().remove::<TenantMember>() else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "require_member did not run before widen_to_branches"
        )));
    };
    let organizations = match member.organizations {
        None => None,
        Some(assigned) => {
            // a system session, because the record session needs this scope to exist
            let tenant = db
                .system_session(&member.database)
                .await
                .map_err(|error| AppError::Internal(error.into()))?;
            let reached = organization::with_branches(&tenant, assigned).await?;
            Some(reached.into_iter().map(RecordId::from).collect())
        }
    };
    request.extensions_mut().insert(TenantAccess {
        database: member.database,
        account: member.account.into(),
        grant: Grant {
            organizations,
            write: member.write,
        },
    });
    Ok(next.run(request).await)
}
