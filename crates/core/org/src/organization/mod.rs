//! Organizations: `model` holds the types and the business rules, `store` the SurrealQL,
//! `http` the routes

mod http;
mod model;
mod store;

pub use http::router;
pub use model::{
    Industry, MAX_DEPTH, NewOrganization, OrganizationError, OrganizationPatch, OrganizationView,
    create, delete, get, list, reassign, update,
};
