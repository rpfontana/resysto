use resysto_model::OrganizationId;
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::SurrealValue;

use super::account::AccountId;
use super::error::AuthError;
use super::mfa::MfaRequiredFor;
use crate::policy::ADMIN;

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
#[derive(Debug, Clone, PartialEq, Serialize)]
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

fn needs_mfa(required_for: MfaRequiredFor, roles: &[String]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::create_account;
    use crate::session::authenticate;
    use crate::testing::{PASSWORD, ada, master, passwords, session_token, sign_in};

    #[tokio::test]
    async fn a_session_reaches_only_the_ready_tenants_of_its_account() {
        let master = master().await;
        master
            .query(
                "CREATE tenant SET slug = 'one', db_name = 't_one', state = 'ready';
                 CREATE tenant SET slug = 'two', db_name = 't_two', state = 'suspended';
                 CREATE tenant SET slug = 'three', db_name = 't_three', state = 'ready';",
            )
            .await
            .unwrap()
            .check()
            .unwrap();
        let admin = vec!["tenant/admin".to_owned()];
        let account = create_account(
            &master,
            &passwords(),
            ada(),
            PASSWORD.into(),
            Some("one"),
            &admin,
        )
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
            ("one", Some("t_one")),
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
