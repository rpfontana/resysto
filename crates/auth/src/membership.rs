use resysto_model::{Email, Member, OrganizationId};
use resysto_surrealdb::{Db, transaction};
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};
use utoipa::ToSchema;

use super::account::AccountId;
use super::audit::{self, About, Event};
use super::error::AuthError;
use super::mfa::MfaRequiredFor;
use crate::policy::{ADMIN, ROLES};

/// A ready tenant an account is a member of
#[derive(Debug, SurrealValue)]
pub(crate) struct Membership {
    pub(crate) database: String,
    /// `None` reaches the whole tenant
    pub(crate) organizations: Option<Vec<OrganizationId>>,
    pub(crate) roles: Vec<String>,
    pub(crate) mfa_required_for: MfaRequiredFor,
}

/// A ready tenant an account is a member of, as the account sees it
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub(crate) struct MemberOf {
    pub(crate) slug: String,
    pub(crate) roles: Vec<String>,
    /// `None` reaches the whole tenant
    pub(crate) organizations: Option<Vec<OrganizationId>>,
    /// Whether the tenant lets the account in only with a second factor
    pub(crate) mfa_required: bool,
}

#[derive(SurrealValue)]
struct MemberRow {
    slug: String,
    roles: Vec<String>,
    organizations: Option<Vec<OrganizationId>>,
    mfa_required_for: MfaRequiredFor,
}

impl Membership {
    pub(crate) fn needs_mfa(&self) -> bool {
        needs_mfa(self.mfa_required_for, &self.roles)
    }
}

pub(crate) fn needs_mfa(required_for: MfaRequiredFor, roles: &[String]) -> bool {
    required_for == MfaRequiredFor::Everyone || roles.iter().any(|role| role == ADMIN)
}

pub(crate) async fn membership(
    master: &Surreal<Any>,
    account: AccountId,
    slug: &str,
) -> Result<Option<Membership>, AuthError> {
    let found: Option<Membership> = master
        .query(
            "SELECT out.db_name AS database, org_scope AS organizations, roles,
                 out.mfa_required_for AS mfa_required_for
             FROM member_of
             WHERE in = $account AND out.slug = $slug AND out.state = 'ready' LIMIT 1",
        )
        .bind(("account", account))
        .bind(("slug", slug.to_owned()))
        .await?
        .check()?
        .take(0)?;
    Ok(found)
}

pub(crate) async fn memberships(
    master: &Surreal<Any>,
    account: AccountId,
) -> Result<Vec<MemberOf>, AuthError> {
    let rows: Vec<MemberRow> = master
        .query(
            "SELECT out.slug AS slug, roles, org_scope AS organizations,
                 out.mfa_required_for AS mfa_required_for
             FROM member_of
             WHERE in = $account AND out.state = 'ready' ORDER BY slug",
        )
        .bind(("account", account))
        .await?
        .check()?
        .take(0)?;
    Ok(rows
        .into_iter()
        .map(|row| MemberOf {
            mfa_required: needs_mfa(row.mfa_required_for, &row.roles),
            slug: row.slug,
            roles: row.roles,
            organizations: row.organizations,
        })
        .collect())
}

#[derive(SurrealValue)]
struct Tenant {
    id: RecordId,
    db_name: String,
}

/// Makes the account a member of the tenant `slug` names, with `roles`; the member's row in the
/// tenant's database goes first, so a failure never leaves a membership whose author the tenant
/// can't name
pub async fn add_member(
    db: &Db,
    master: &Surreal<Any>,
    account: AccountId,
    slug: &str,
    roles: &[String],
) -> Result<(), AuthError> {
    if let Some(unknown) = roles.iter().find(|role| !ROLES.contains(&role.as_str())) {
        return Err(AuthError::UnknownRole(unknown.clone()));
    }
    let tenant: Option<Tenant> = master
        .query("SELECT id, db_name FROM tenant WHERE slug = $slug LIMIT 1")
        .bind(("slug", slug.to_owned()))
        .await?
        .check()?
        .take(0)?;
    let tenant = tenant.ok_or_else(|| AuthError::UnknownTenant(slug.to_owned()))?;
    let email: Option<Email> = master
        .query("SELECT VALUE email FROM ONLY $account")
        .bind(("account", account))
        .await?
        .check()?
        .take(0)?;
    let email = email.ok_or_else(|| AuthError::UnknownAccount(account.to_string()))?;
    db.system_session(&tenant.db_name)
        .await?
        .query("UPSERT $member SET email = $email RETURN NONE")
        .bind(("member", account.cast::<Member>()))
        .bind(("email", email))
        .await?
        .check()?;
    transaction(master, |tx| {
        let (tenant, roles) = (tenant.id.clone(), roles.to_vec());
        async move {
            let about = About {
                tenant: Some(tenant.clone()),
                ..About::of(account)
            };
            tx.query("RELATE $account->member_of->$tenant SET roles = $roles RETURN NONE")
                .bind(("account", account))
                .bind(("tenant", tenant))
                .bind(("roles", roles))
                .await?
                .check()?;
            audit::record(&tx, Event::MemberAdded, about).await?;
            Ok(())
        }
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::create_account;
    use crate::session::authenticate;
    use crate::testing::{
        PASSWORD, ada, new_master, new_tenant, passwords, server, session_token, sign_in,
    };

    #[tokio::test]
    async fn a_new_member_is_named_in_its_tenant_and_recorded_in_the_master() {
        let server = server().await;
        let (_, master) = new_master(&server).await;
        let one = new_tenant(&server, &master, "one").await;
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();
        let reader = vec!["tenant/reader".to_owned()];

        let owner = vec!["tenant/owner".to_owned()];
        let unknown_role = add_member(&server, &master, account, "one", &owner).await;
        assert!(
            matches!(unknown_role, Err(AuthError::UnknownRole(_))),
            "{:?}",
            unknown_role.err()
        );
        let unknown_tenant = add_member(&server, &master, account, "nobody", &reader).await;
        assert!(
            matches!(unknown_tenant, Err(AuthError::UnknownTenant(_))),
            "{:?}",
            unknown_tenant.err()
        );
        add_member(&server, &master, account, "one", &reader)
            .await
            .unwrap();

        let email: Option<Email> = server
            .system_session(&one)
            .await
            .unwrap()
            .query("SELECT VALUE email FROM ONLY $member")
            .bind(("member", account.cast::<Member>()))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(email, Some(ada()));
        let reachable = memberships(&master, account).await.unwrap();
        let roles: Vec<&[String]> = reachable.iter().map(|member| &member.roles[..]).collect();
        assert_eq!(roles, [&reader[..]]);
        let added: Vec<String> = master
            .query("SELECT VALUE event FROM audit_event WHERE tenant != NONE")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(added, ["member_added"]);
    }

    #[tokio::test]
    async fn a_session_reaches_only_the_ready_tenants_of_its_account() {
        let server = server().await;
        let (_, master) = new_master(&server).await;
        let one = new_tenant(&server, &master, "one").await;
        master
            .query(
                "CREATE tenant SET slug = 'two', db_name = 't_two', state = 'suspended';
                 CREATE tenant SET slug = 'three', db_name = 't_three', state = 'ready';",
            )
            .await
            .unwrap()
            .check()
            .unwrap();
        let admin = vec!["tenant/admin".to_owned()];
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();
        add_member(&server, &master, account, "one", &admin)
            .await
            .unwrap();
        let branch = OrganizationId::new();
        master
            .query(
                "LET $two = (SELECT VALUE id FROM tenant WHERE slug = 'two')[0];
                 RELATE $account->member_of->$two SET roles = $roles;
                 UPDATE member_of SET org_scope = [$branch] WHERE out.slug = 'one';",
            )
            .bind(("account", account))
            .bind(("roles", admin.clone()))
            .bind(("branch", branch))
            .await
            .unwrap()
            .check()
            .unwrap();
        let token = session_token(sign_in(&master, ada()).await);

        let signed_in = authenticate(&master, &token)
            .await
            .unwrap()
            .unwrap()
            .account;
        let cases = [
            ("one", Some(one.as_str())),
            ("two", None),
            ("three", None),
            ("nobody", None),
        ];
        for (slug, database) in cases {
            let found = membership(&master, signed_in, slug).await.unwrap();
            let found = found.as_ref().map(|member| member.database.as_str());
            assert_eq!(found, database, "slug {slug}");
        }
        let scope = membership(&master, signed_in, "one")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scope.organizations, Some(vec![branch]));
        assert_eq!(scope.roles, admin);

        let reachable = memberships(&master, signed_in).await.unwrap();
        let one = MemberOf {
            slug: "one".into(),
            roles: admin,
            organizations: Some(vec![branch]),
            mfa_required: true,
        };
        assert_eq!(reachable, vec![one]);
    }
}
