use std::borrow::Cow;

use resysto_id::{Entity, Id};
use serde::Serialize;
use surrealdb_types::{Datetime, SurrealValue, Value};
use utoipa::openapi::schema::{
    AllOfBuilder, KnownFormat, ObjectBuilder, OneOfBuilder, Schema, SchemaFormat, Type,
};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{PartialSchema, ToSchema};

use crate::MemberId;

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
    /// `None` when the system wrote it, or before writes were recorded
    pub created_by: Option<MemberId>,
    pub updated_at: Datetime,
    /// `None` when the system wrote it, or before writes were recorded
    pub updated_by: Option<MemberId>,
    #[serde(flatten)]
    #[surreal(flatten)]
    pub data: T,
}

// written by hand because the derive names every record `Record`, whatever its data: here
// `Record<_, Person>` is `PersonRecord`
impl<E: Entity, T: SurrealValue + ToSchema> utoipa::__dev::ComposeSchema for Record<E, T> {
    fn compose(_: Vec<RefOr<Schema>>) -> RefOr<Schema> {
        let datetime = || {
            ObjectBuilder::new()
                .schema_type(Type::String)
                .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
        };
        let member = || {
            OneOfBuilder::new()
                .item(Ref::from_schema_name(MemberId::name()))
                .item(ObjectBuilder::new().schema_type(Type::Null))
                .description(Some("`null` when the system wrote it"))
        };
        let written = ObjectBuilder::new()
            .property("id", Id::<E>::schema())
            .property(
                "version",
                ObjectBuilder::new()
                    .schema_type(Type::Integer)
                    .format(Some(SchemaFormat::KnownFormat(KnownFormat::Int64)))
                    .description(Some(
                        "Raised on every write; the ETag, which a write sends back in If-Match",
                    )),
            )
            .property("created_at", datetime())
            .property("created_by", member())
            .property("updated_at", datetime())
            .property("updated_by", member())
            .required("id")
            .required("version")
            .required("created_at")
            .required("created_by")
            .required("updated_at")
            .required("updated_by");
        AllOfBuilder::new()
            .item(written)
            .item(Ref::from_schema_name(T::name()))
            .into()
    }
}

impl<E: Entity, T: SurrealValue + ToSchema> ToSchema for Record<E, T> {
    fn name() -> Cow<'static, str> {
        Cow::Owned(format!("{}Record", T::name()))
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((T::name().into_owned(), T::schema()));
        T::schemas(schemas);
        schemas.push((MemberId::name().into_owned(), MemberId::schema()));
    }
}
