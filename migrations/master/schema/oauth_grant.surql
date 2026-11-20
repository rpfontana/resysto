-- a member's consent to an agent's app; deleting it deletes its tokens
DEFINE TABLE OVERWRITE oauth_grant SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE account ON oauth_grant TYPE record<account> REFERENCE ON DELETE CASCADE;
-- the client_id: the URL of the app's metadata document
DEFINE FIELD OVERWRITE client ON oauth_grant TYPE string;
DEFINE FIELD OVERWRITE client_name ON oauth_grant TYPE string;
-- the SHA-256 of its authorization code, to spot a code used twice
DEFINE FIELD OVERWRITE code ON oauth_grant TYPE option<string>;
DEFINE FIELD OVERWRITE created_at ON oauth_grant TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE expires_at ON oauth_grant TYPE datetime;
DEFINE FIELD OVERWRITE mfa ON oauth_grant TYPE bool;
DEFINE FIELD OVERWRITE tenants ON oauth_grant TYPE array<record<tenant>> REFERENCE ON DELETE UNSET;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_oauth_grant_account ON oauth_grant FIELDS account;
DEFINE INDEX OVERWRITE idx_oauth_grant_code ON oauth_grant FIELDS code UNIQUE;
