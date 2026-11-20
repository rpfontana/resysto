use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use surrealdb_types::{Kind, SurrealValue, Value, conversion_error};
use utoipa::ToSchema;

/// ISO 3166-1 alpha-2 codes, sorted
const CODES: [&str; 249] = [
    "AD", "AE", "AF", "AG", "AI", "AL", "AM", "AO", "AQ", "AR", "AS", "AT", "AU", "AW", "AX", "AZ",
    "BA", "BB", "BD", "BE", "BF", "BG", "BH", "BI", "BJ", "BL", "BM", "BN", "BO", "BQ", "BR", "BS",
    "BT", "BV", "BW", "BY", "BZ", "CA", "CC", "CD", "CF", "CG", "CH", "CI", "CK", "CL", "CM", "CN",
    "CO", "CR", "CU", "CV", "CW", "CX", "CY", "CZ", "DE", "DJ", "DK", "DM", "DO", "DZ", "EC", "EE",
    "EG", "EH", "ER", "ES", "ET", "FI", "FJ", "FK", "FM", "FO", "FR", "GA", "GB", "GD", "GE", "GF",
    "GG", "GH", "GI", "GL", "GM", "GN", "GP", "GQ", "GR", "GS", "GT", "GU", "GW", "GY", "HK", "HM",
    "HN", "HR", "HT", "HU", "ID", "IE", "IL", "IM", "IN", "IO", "IQ", "IR", "IS", "IT", "JE", "JM",
    "JO", "JP", "KE", "KG", "KH", "KI", "KM", "KN", "KP", "KR", "KW", "KY", "KZ", "LA", "LB", "LC",
    "LI", "LK", "LR", "LS", "LT", "LU", "LV", "LY", "MA", "MC", "MD", "ME", "MF", "MG", "MH", "MK",
    "ML", "MM", "MN", "MO", "MP", "MQ", "MR", "MS", "MT", "MU", "MV", "MW", "MX", "MY", "MZ", "NA",
    "NC", "NE", "NF", "NG", "NI", "NL", "NO", "NP", "NR", "NU", "NZ", "OM", "PA", "PE", "PF", "PG",
    "PH", "PK", "PL", "PM", "PN", "PR", "PS", "PT", "PW", "PY", "QA", "RE", "RO", "RS", "RU", "RW",
    "SA", "SB", "SC", "SD", "SE", "SG", "SH", "SI", "SJ", "SK", "SL", "SM", "SN", "SO", "SR", "SS",
    "ST", "SV", "SX", "SY", "SZ", "TC", "TD", "TF", "TG", "TH", "TJ", "TK", "TL", "TM", "TN", "TO",
    "TR", "TT", "TV", "TW", "TZ", "UA", "UG", "UM", "US", "UY", "UZ", "VA", "VC", "VE", "VG", "VI",
    "VN", "VU", "WF", "WS", "YE", "YT", "ZA", "ZM", "ZW",
];

/// An ISO 3166-1 alpha-2 country code
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ToSchema)]
#[schema(value_type = String, example = "IT")]
pub struct Country(&'static str);

/// A value that is not an ISO 3166-1 alpha-2 code
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseCountryError;

impl Country {
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl FromStr for Country {
    type Err = ParseCountryError;

    /// Reads a code in any case, ignoring surrounding spaces
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let code = raw.trim().to_ascii_uppercase();
        CODES
            .binary_search(&code.as_str())
            .ok()
            .and_then(|index| CODES.get(index).copied())
            .map(Self)
            .ok_or(ParseCountryError)
    }
}

impl AsRef<str> for Country {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl fmt::Display for Country {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Serialize for Country {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for Country {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl SurrealValue for Country {
    fn kind_of() -> Kind {
        Kind::String
    }

    fn into_value(self) -> Value {
        Value::String(self.0.to_owned())
    }

    fn from_value(value: Value) -> Result<Self, surrealdb_types::Error> {
        if let Value::String(raw) = &value
            && let Ok(country) = raw.parse()
        {
            return Ok(country);
        }
        Err(conversion_error(Self::kind_of(), value))
    }
}

impl fmt::Display for ParseCountryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an ISO 3166-1 alpha-2 country code")
    }
}

impl Error for ParseCountryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_stay_sorted_for_the_binary_search() {
        assert!(CODES.is_sorted(), "CODES is out of order");
    }

    #[test]
    fn reads_codes_in_any_case_and_refuses_the_rest() {
        for (raw, code) in [("IT", "IT"), ("it", "IT"), (" De ", "DE")] {
            assert_eq!(raw.parse().map(Country::as_str), Ok(code), "{raw:?}");
        }
        for raw in ["", "XX", "ITA", "I", "Italy"] {
            assert!(raw.parse::<Country>().is_err(), "accepted {raw:?}");
        }
        assert!(serde_json::from_str::<Country>("\"XX\"").is_err());
    }
}
