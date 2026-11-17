use std::sync::Arc;

use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};
use secrecy::{ExposeSecret, SecretString};
use unicode_normalization::UnicodeNormalization;

const MIN_LENGTH: usize = 12;
const MAX_LENGTH: usize = 128;

#[derive(Clone)]
pub struct Passwords(Arc<Inner>);

struct Inner {
    pepper: SecretString,
    // checked when the email is unknown/wrong
    dummy: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("a password needs from {MIN_LENGTH} to {MAX_LENGTH} characters")]
    Length,
    #[error("the pepper does not fit Argon2: {0}")]
    Pepper(argon2::Error),
    #[error("hashing failed: {0}")]
    Hash(argon2::password_hash::Error),
}

impl Passwords {
    pub fn new(pepper: SecretString) -> Result<Self, PasswordError> {
        let unknown: [u8; 32] = rand::random();
        let dummy = argon2(&pepper)?
            .hash_password(&unknown)
            .map_err(PasswordError::Hash)?
            .to_string();
        Ok(Self(Arc::new(Inner { pepper, dummy })))
    }

    pub(crate) fn hash(&self, password: &str) -> Result<String, PasswordError> {
        let password = normalized(password);
        if !(MIN_LENGTH..=MAX_LENGTH).contains(&password.chars().count()) {
            return Err(PasswordError::Length);
        }
        self.hash_code(&password)
    }

    /// Hashes a recovery code, random enough to need no length rule; `verify` checks it
    pub(crate) fn hash_code(&self, code: &str) -> Result<String, PasswordError> {
        Ok(argon2(&self.0.pepper)?
            .hash_password(code.as_bytes())
            .map_err(PasswordError::Hash)?
            .to_string())
    }

    /// Whether `password` matches `hash`; with no hash it spends the same time and fails
    pub(crate) fn verify(&self, password: &str, hash: Option<&str>) -> bool {
        let Ok(argon2) = argon2(&self.0.pepper) else {
            return false;
        };
        let matches = argon2
            .verify_password(
                normalized(password).as_bytes(),
                hash.unwrap_or(&self.0.dummy),
            )
            .is_ok();
        matches && hash.is_some()
    }
}

fn argon2(pepper: &SecretString) -> Result<Argon2<'_>, PasswordError> {
    Argon2::new_with_secret(
        pepper.expose_secret().as_bytes(),
        Algorithm::Argon2id,
        Version::V0x13,
        Params::default(),
    )
    .map_err(PasswordError::Pepper)
}

fn normalized(password: &str) -> String {
    password.nfkc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passwords(pepper: &str) -> Passwords {
        Passwords::new(SecretString::from(pepper)).unwrap()
    }

    #[test]
    fn a_password_matches_its_compatibility_form_and_only_with_its_pepper() {
        let hash = passwords("pepper").hash("ﬁfteen characters").unwrap();

        assert!(passwords("pepper").verify("fifteen characters", Some(&hash)));
        assert!(!passwords("other").verify("ﬁfteen characters", Some(&hash)));
        assert!(!passwords("pepper").verify("ﬁfteen characters", None));
    }

    #[test]
    fn a_new_password_needs_15_to_128_characters() {
        let passwords = passwords("pepper");
        for (length, accepted) in [(14, false), (15, true), (128, true), (129, false)] {
            let hashed = passwords.hash(&"è".repeat(length));
            assert_eq!(hashed.is_ok(), accepted, "{length} characters");
        }
    }
}
