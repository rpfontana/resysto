//! Organizations: `model` holds the types and the business rules, `ops` the operations,
//! `store` the SurrealQL, `http` the routes, `mcp` the tools for agents

mod http;
mod mcp;
mod model;
mod ops;
mod store;

pub use http::router;
pub use mcp::tools;
pub use ops::{OrganizationError, with_branches};
