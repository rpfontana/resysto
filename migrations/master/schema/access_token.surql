-- the record key is the SHA-256 of the token, never the token
DEFINE TABLE OVERWRITE access_token SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE created_at ON access_token TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE expires_at ON access_token TYPE datetime;
DEFINE FIELD OVERWRITE grant ON access_token TYPE record<oauth_grant> REFERENCE ON DELETE CASCADE;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_access_token_grant ON access_token FIELDS grant;
