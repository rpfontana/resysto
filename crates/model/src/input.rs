use serde::{Deserialize, Deserializer};
use validator::ValidationError;

/// Deserializes a field a patch may set but not clear: absent is `None`, `null` is refused
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Deserializes a field a patch may clear: absent is `None`, `null` is `Some(None)`
pub fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Refuses a text that is empty once trimmed: `null` is the one way to clear a field
pub fn not_blank(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        Err(ValidationError::new("blank").with_message("must not be blank, null clears it".into()))
    } else {
        Ok(())
    }
}

/// The text without the spaces around it
pub fn trimmed(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_owned())
}
