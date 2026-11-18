DEFINE TABLE OVERWRITE account SCHEMAFULL PERMISSIONS NONE;

DEFINE FIELD OVERWRITE email ON account TYPE string;
DEFINE FIELD OVERWRITE password_hash ON account TYPE string;
DEFINE FIELD OVERWRITE active ON account TYPE bool DEFAULT true;
DEFINE FIELD OVERWRITE created_at ON account TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON account TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE idx_account_email ON account FIELDS email UNIQUE;
REMOVE INDEX IF EXISTS account_email ON account;

-- NONE while the account has no authenticator app
DEFINE FIELD OVERWRITE totp ON account TYPE option<object>;
-- sealed with AES-256-GCM under the server's encryption key
DEFINE FIELD OVERWRITE totp.secret ON account TYPE bytes;
-- the time step of the last code accepted, so a code works once
DEFINE FIELD OVERWRITE totp.last_step ON account TYPE int;
-- Argon2id hashes of the recovery codes not used yet
DEFINE FIELD OVERWRITE totp.recovery_codes ON account TYPE array<string>;
-- a sealed secret waiting for its first code
DEFINE FIELD OVERWRITE totp_pending ON account TYPE option<bytes>;
