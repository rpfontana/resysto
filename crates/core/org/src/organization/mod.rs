//! Organizations: `model` holds the types and the business rules, `ops` the operations,
//! `store` the SurrealQL, `http` the routes

mod http;
mod model;
mod ops;
mod store;

pub use http::router;
pub use model::{
    Industry, MAX_DEPTH, NewOrganization, Organization, OrganizationPatch, OrganizationRecord,
};
pub use ops::{OrganizationError, create, delete, get, list, reassign, update, with_branches};
