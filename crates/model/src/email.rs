use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use surrealdb_types::{Kind, SurrealValue, Value, conversion_error};
use utoipa::ToSchema;
use validator::ValidateEmail;

/// An email address, trimmed and lowercased
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ToSchema)]
#[schema(format = Email, example = "mario.rossi@acme.it")]
pub struct Email(String);

/// A value that is not an email address
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseEmailError;

impl Email {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Email {
    type Err = ParseEmailError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let email = raw.trim().to_lowercase();
        if email.validate_email() {
            Ok(Self(email))
        } else {
            Err(ParseEmailError)
        }
    }
}

impl AsRef<str> for Email {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Email> for String {
    fn from(email: Email) -> Self {
        email.0
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Email {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Email {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl SurrealValue for Email {
    fn kind_of() -> Kind {
        Kind::String
    }

    fn into_value(self) -> Value {
        Value::String(self.0)
    }

    fn from_value(value: Value) -> Result<Self, surrealdb_types::Error> {
        if let Value::String(raw) = &value
            && let Ok(email) = raw.parse()
        {
            return Ok(email);
        }
        Err(conversion_error(Self::kind_of(), value))
    }
}

impl fmt::Display for ParseEmailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an email address")
    }
}

impl Error for ParseEmailError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_lowercases_and_parses_on_the_way_in() {
        let email: Email = serde_json::from_str("\" Mario.Rossi@Acme.IT \"").unwrap();
        assert_eq!(email.as_str(), "mario.rossi@acme.it");
        assert!(serde_json::from_str::<Email>("\"mario.rossi\"").is_err());
        let stored = Value::String("mario.rossi".into());
        assert!(
            Email::from_value(stored).is_err(),
            "read a bad address from the database"
        );
    }
}
