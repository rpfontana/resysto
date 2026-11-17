use std::collections::{HashMap, HashSet};
use std::error::Error;

use axum::http::Method;
use cedar_policy::{
    Authorizer, Context, Decision, Entities, Entity, EntityId, EntityTypeName, EntityUid,
    ParseErrors, PolicySet, Request, RestrictedExpression, Schema, ValidationMode, Validator,
};

use super::account::AccountId;

const SCHEMA: &str = include_str!("../policies/resysto.cedarschema");
const POLICIES: &str = include_str!("../policies/roles.cedar");

pub(crate) const ADMIN: &str = "tenant/admin";
/// The roles the policies grant something to in a tenant
pub(crate) const ROLES: [&str; 3] = [ADMIN, "tenant/analyst", "tenant/reader"];

/// The Cedar schema and the policies, validated against each other
#[derive(Debug)]
pub struct Policies {
    schema: Schema,
    set: PolicySet,
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("the Cedar schema is invalid: {0}")]
    Schema(String),
    #[error("the Cedar policies do not parse: {0}")]
    Parse(String),
    #[error("the Cedar policies do not match the schema: {0}")]
    Validation(String),
}

/// A member of a tenant, as the policies see it
#[derive(Debug)]
pub(crate) struct Member {
    pub(crate) account: AccountId,
    pub(crate) roles: Vec<String>,
    pub(crate) whole_tenant: bool,
    pub(crate) tenant: String,
}

/// What a request does to its resource, from its HTTP method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Read,
    Write,
    Delete,
}

impl Policies {
    pub fn load() -> Result<Self, PolicyError> {
        let (schema, _) = Schema::from_cedarschema_str(SCHEMA)
            .map_err(|error| PolicyError::Schema(error.to_string()))?;
        let set: PolicySet = POLICIES
            .parse()
            .map_err(|error: ParseErrors| PolicyError::Parse(error.to_string()))?;
        let validation = Validator::new(schema.clone()).validate(&set, ValidationMode::Strict);
        if !validation.validation_passed() {
            let errors: Vec<String> = validation
                .validation_errors()
                .map(ToString::to_string)
                .collect();
            return Err(PolicyError::Validation(errors.join("; ")));
        }
        Ok(Self { schema, set })
    }

    /// Whether the member may do `verb` to the REST resource `resource`, such as `people`
    pub(crate) fn allows(&self, member: &Member, verb: Verb, resource: &str) -> bool {
        match self.decide(member, verb, resource) {
            Ok(decision) => decision == Decision::Allow,
            Err(error) => {
                tracing::error!(%error, resource, "authorization request failed");
                false
            }
        }
    }

    fn decide(
        &self,
        member: &Member,
        verb: Verb,
        resource: &str,
    ) -> Result<Decision, Box<dyn Error>> {
        let roles = member
            .roles
            .iter()
            .map(|role| uid("Role", role))
            .collect::<Result<HashSet<_>, _>>()?;
        let principal = uid("Account", &member.account.to_string())?;
        let tenant = uid("Tenant", &member.tenant)?;
        let target = uid("Resource", resource)?;
        let mut entities: Vec<Entity> = roles
            .iter()
            .map(|role| Entity::new_no_attrs(role.clone(), HashSet::new()))
            .collect();
        let whole_tenant = RestrictedExpression::new_bool(member.whole_tenant);
        let attrs = HashMap::from([("whole_tenant".to_owned(), whole_tenant)]);
        entities.push(Entity::new(principal.clone(), attrs, roles)?);
        entities.push(Entity::new_no_attrs(
            target.clone(),
            HashSet::from([tenant.clone()]),
        ));
        entities.push(Entity::new_no_attrs(tenant, HashSet::new()));
        let schema = Some(&self.schema);
        let entities = Entities::from_entities(entities, schema)?;
        let request = Request::new(
            principal,
            uid("Action", verb.name())?,
            target,
            Context::empty(),
            schema,
        )?;
        let response = Authorizer::new().is_authorized(&request, &self.set, &entities);
        Ok(response.decision())
    }
}

impl Verb {
    pub(crate) fn of(method: &Method) -> Self {
        match *method {
            Method::GET | Method::HEAD | Method::OPTIONS => Self::Read,
            Method::DELETE => Self::Delete,
            _ => Self::Write,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }
}

fn uid(kind: &str, id: &str) -> Result<EntityUid, Box<dyn Error>> {
    let kind: EntityTypeName = format!("Resysto::{kind}").parse()?;
    Ok(EntityUid::from_type_name_and_id(kind, EntityId::new(id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_role_does_what_its_policies_say_and_nothing_else() {
        let policies = Policies::load().unwrap();
        let member = |roles: &[&str], whole_tenant: bool| Member {
            account: AccountId::new(),
            roles: roles.iter().map(ToString::to_string).collect(),
            whole_tenant,
            tenant: "t_acme".into(),
        };
        let requests = [
            (Verb::Read, "organizations"),
            (Verb::Write, "organizations"),
            (Verb::Delete, "organizations"),
            (Verb::Read, "people"),
            (Verb::Write, "people"),
            (Verb::Delete, "people"),
        ];
        let cases = [
            (member(&["tenant/admin"], true), [true; 6]),
            (
                member(&["tenant/admin"], false),
                [true, false, false, true, true, true],
            ),
            (
                member(&["tenant/analyst"], true),
                [true, false, false, true, true, true],
            ),
            (
                member(&["tenant/reader"], true),
                [true, false, false, true, false, false],
            ),
            (member(&[], true), [false; 6]),
            (member(&["tenant/owner"], true), [false; 6]),
        ];
        for (member, allowed) in cases {
            for ((verb, resource), allowed) in requests.into_iter().zip(allowed) {
                let got = policies.allows(&member, verb, resource);
                assert_eq!(got, allowed, "{member:?} {verb:?} {resource}");
            }
        }
        for role in ROLES {
            let member = member(&[role], true);
            let any = requests
                .iter()
                .any(|(verb, resource)| policies.allows(&member, *verb, resource));
            assert!(any, "{role} may do nothing");
        }
    }
}
