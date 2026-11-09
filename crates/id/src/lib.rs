//! Entity-typed UUID v7 identifiers.

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// A type stored in its own table.
///
/// ```rust
/// # use resysto_id::{Entity, Id};
/// pub enum Person {}
///
/// impl Entity for Person {
///     const TABLE: &'static str = "person";
/// }
///
/// pub type PersonId = Id<Person>;
/// ```
pub trait Entity: 'static {
    /// Table name.
    const TABLE: &'static str;
}

/// UUID v7 identifier of a `T` record.
///
/// Serialized as a plain UUID string. Parsing rejects other UUID versions.
pub struct Id<T> {
    key: Uuid,
    kind: PhantomData<fn() -> T>,
}

/// The error returned when converting to an [`Id`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseIdError;

impl<T> Id<T> {
    /// Generates an id from the current time. Not available on `wasm32`.
    #[cfg(not(target_arch = "wasm32"))]
    #[expect(
        clippy::new_without_default,
        reason = "a default id would be a fresh random one, created by accident"
    )]
    #[must_use]
    pub fn new() -> Self {
        Self {
            key: Uuid::now_v7(),
            kind: PhantomData,
        }
    }

    fn from_key(key: Uuid) -> Result<Self, ParseIdError> {
        if key.get_version_num() == 7 {
            Ok(Self {
                key,
                kind: PhantomData,
            })
        } else {
            Err(ParseIdError)
        }
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl<T: Entity> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", T::TABLE, self.key)
    }
}

impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.key.fmt(f)
    }
}

impl<T> FromStr for Id<T> {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s)
            .map_err(|_| ParseIdError)
            .and_then(Self::from_key)
    }
}

impl<T> Serialize for Id<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.key)
    }
}

impl<'de, T> Deserialize<'de> for Id<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ParseIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid id, expected a version 7 uuid of this entity")
    }
}

impl Error for ParseIdError {}

#[cfg(not(target_arch = "wasm32"))]
mod surreal {
    use surrealdb_types::{
        Error, Kind, RecordId, RecordIdKey, SurrealValue, Table, Uuid, Value, conversion_error,
    };

    use super::{Entity, Id, ParseIdError};

    impl<T: Entity> From<Id<T>> for RecordId {
        fn from(id: Id<T>) -> Self {
            Self::new(T::TABLE, Uuid::from(id.key))
        }
    }

    impl<T: Entity> TryFrom<&RecordId> for Id<T> {
        type Error = ParseIdError;

        fn try_from(record: &RecordId) -> Result<Self, Self::Error> {
            match &record.key {
                RecordIdKey::Uuid(key) if record.table.as_str() == T::TABLE => {
                    Self::from_key(key.into_inner())
                }
                _ => Err(ParseIdError),
            }
        }
    }

    impl<T: Entity> SurrealValue for Id<T> {
        fn kind_of() -> Kind {
            Kind::Record(vec![Table::new(T::TABLE)])
        }

        fn into_value(self) -> Value {
            Value::RecordId(self.into())
        }

        fn from_value(value: Value) -> Result<Self, Error> {
            if let Value::RecordId(record) = &value
                && let Ok(id) = Self::try_from(record)
            {
                return Ok(id);
            }
            Err(conversion_error(Self::kind_of(), value))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        enum Person {}

        impl Entity for Person {
            const TABLE: &'static str = "person";
        }

        #[test]
        fn round_trips_as_a_record_of_its_table() {
            let id = Id::<Person>::new();
            let value = id.into_value();
            assert!(
                matches!(&value, Value::RecordId(r) if r.table.as_str() == "person"),
                "wrote {value:?}"
            );
            assert_eq!(Id::<Person>::from_value(value).unwrap(), id);
        }

        #[test]
        fn rejects_records_that_are_not_its_own() {
            let cases = [
                RecordId::new("task", Uuid::new_v7()),
                RecordId::new("person", Uuid::new_v4()),
                RecordId::new("person", RecordIdKey::String(Uuid::new_v7().to_string())),
            ];
            for record in cases {
                assert!(
                    Id::<Person>::from_value(Value::RecordId(record.clone())).is_err(),
                    "accepted {record:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Person {}

    impl Entity for Person {
        const TABLE: &'static str = "person";
    }

    const V4: &str = "936da01f-9abd-4d9d-80c7-02af85c822a8";

    #[test]
    fn parses_only_version_7_uuids() {
        let v7 = Id::<Person>::new().to_string();
        assert!(v7.parse::<Id<Person>>().is_ok(), "rejected {v7:?}");

        let prefixed = format!("person:{v7}");
        let rejected = [
            "",
            "not-a-uuid",
            V4,
            "00000000-0000-0000-0000-000000000000",
            prefixed.as_str(),
        ];
        for raw in rejected {
            assert!(raw.parse::<Id<Person>>().is_err(), "accepted {raw:?}");
        }
    }

    #[test]
    fn travels_as_a_bare_uuid_and_is_checked_on_the_way_in() {
        let id = Id::<Person>::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<Id<Person>>(&json).unwrap(), id);

        let v4 = format!("\"{V4}\"");
        assert!(
            serde_json::from_str::<Id<Person>>(&v4).is_err(),
            "accepted {v4}"
        );
    }
}
