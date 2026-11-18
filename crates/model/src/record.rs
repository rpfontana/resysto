use resysto_id::{Entity, Id};
use serde::Serialize;
use surrealdb_types::{Datetime, SurrealValue, Value};

/// A row as the database keeps it: the entity's data and the fields only the server writes
///
/// Flat on the wire and in the database: `data`'s fields sit next to `id` and `version`.
#[derive(Debug, Serialize, SurrealValue)]
#[serde(bound(serialize = "T: Serialize"))]
#[surreal(crate = "surrealdb_types")]
pub struct Record<E: Entity, T: SurrealValue> {
    pub id: Id<E>,
    /// Raised by the database on every write; clients send it back in `If-Match`
    pub version: i64,
    pub created_at: Datetime,
    pub updated_at: Datetime,
    #[serde(flatten)]
    #[surreal(flatten)]
    pub data: T,
}
