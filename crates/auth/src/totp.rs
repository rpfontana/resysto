use std::sync::Arc;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use rand::seq::IndexedRandom;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use totp_rs::{Builder, Secret, Totp, TotpError};

use super::account::AccountId;

// 160 bits
const SECRET_BYTES: usize = 20;
/// The name an authenticator app shows next to the codes
const ISSUER: &str = "Resysto";
const NONCE_BYTES: usize = 12;
const RECOVERY_CODES: usize = 10;
/// Ten characters of a 31-letter alphabet: about 50 random bits per code
const RECOVERY_CODE_LENGTH: usize = 10;
/// No 0, 1, i, l, o: nothing to mistake when typing a code from paper
const RECOVERY_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// Seals TOTP secrets with AES-256-GCM
#[derive(Clone)]
pub struct Cipher(Arc<Aes256Gcm>);

#[derive(Debug, thiserror::Error)]
pub enum CipherError {
    #[error("the encryption key must be 64 hex digits")]
    Key,
    #[error("sealing a secret failed")]
    Seal,
    #[error("a sealed secret does not open with the encryption key")]
    Open,
}

/// What an authenticator app needs to start producing codes
#[derive(Debug, Serialize)]
pub(crate) struct TotpSetup {
    /// Base32, for typing into the app
    pub(crate) secret: String,
    /// The `otpauth://` URI a QR code carries
    pub(crate) uri: String,
}

impl Cipher {
    pub fn new(key: &SecretString) -> Result<Self, CipherError> {
        let key: [u8; 32] = hex::decode(key.expose_secret().trim())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(CipherError::Key)?;
        Ok(Self(Arc::new(Aes256Gcm::new(&Key::<Aes256Gcm>::from(key)))))
    }

    // the account is the associated data: a secret copied onto another account does not open
    pub(super) fn seal(&self, account: AccountId, secret: &[u8]) -> Result<Vec<u8>, CipherError> {
        let nonce: [u8; NONCE_BYTES] = rand::random();
        let aad = account.to_string();
        let payload = Payload {
            msg: secret,
            aad: aad.as_bytes(),
        };
        let sealed = self
            .0
            .encrypt(&Nonce::from(nonce), payload)
            .map_err(|_| CipherError::Seal)?;
        Ok([nonce.as_slice(), &sealed].concat())
    }

    pub(super) fn open(&self, account: AccountId, sealed: &[u8]) -> Result<Vec<u8>, CipherError> {
        let (nonce, sealed) = sealed
            .split_first_chunk::<NONCE_BYTES>()
            .ok_or(CipherError::Open)?;
        let aad = account.to_string();
        let payload = Payload {
            msg: sealed,
            aad: aad.as_bytes(),
        };
        self.0
            .decrypt(&Nonce::from(*nonce), payload)
            .map_err(|_| CipherError::Open)
    }
}

pub(super) fn new_secret() -> [u8; SECRET_BYTES] {
    rand::random()
}

pub(super) fn setup(secret: &[u8], email: &str) -> Result<TotpSetup, TotpError> {
    let totp = Builder::new()
        .with_secret(secret)
        .with_account_name(email)
        .with_issuer(Some(ISSUER))
        .build()?;
    Ok(TotpSetup {
        secret: Secret::from(secret).to_base32(),
        uri: totp.to_url()?,
    })
}

/// The time step `code` is right for, within one step of the server's clock either side; the
/// defaults are what every authenticator app does: HMAC-SHA1, 6 digits, 30-second steps
pub(super) fn step_of(secret: &[u8], code: &str) -> Result<Option<i64>, TotpError> {
    let totp: Totp = Builder::new().with_secret(secret).build()?;
    let code: String = code.split_whitespace().collect();
    Ok(totp
        .check_current(&code)
        .and_then(|step| i64::try_from(step).ok()))
}

pub(super) fn recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODES).map(|_| recovery_code()).collect()
}

fn recovery_code() -> String {
    let mut rng = rand::rng();
    let letters: String = (0..RECOVERY_CODE_LENGTH)
        .filter_map(|_| RECOVERY_ALPHABET.choose(&mut rng).copied().map(char::from))
        .collect();
    let (left, right) = letters.split_at(RECOVERY_CODE_LENGTH / 2);
    format!("{left}-{right}")
}

/// The form a recovery code is hashed in
pub(super) fn normalized_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_secret_opens_only_for_its_account_and_with_its_key() {
        let key = |digit: &str| Cipher::new(&SecretString::from(digit.repeat(64))).unwrap();
        let (cipher, account, other) = (key("1"), AccountId::new(), AccountId::new());
        let sealed = cipher.seal(account, b"secret").unwrap();

        assert_eq!(cipher.open(account, &sealed).unwrap(), b"secret");
        assert!(cipher.open(other, &sealed).is_err());
        assert!(key("2").open(account, &sealed).is_err());
    }
}
