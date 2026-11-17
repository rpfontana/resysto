use sha2::{Digest, Sha256};

/// A session cookie's value, or a challenge's; the database keeps only its SHA-256
pub(crate) struct Token(String);

impl Token {
    pub(super) fn new() -> Self {
        let bytes: [u8; 32] = rand::random();
        Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }

    pub(super) fn key(&self) -> String {
        key(&self.0)
    }
}

pub(super) fn key(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
