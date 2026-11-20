-- the record key is the SHA-256 of the token; each one works once
DEFINE TABLE OVERWRITE refresh_token SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE created_at ON refresh_token TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE expires_at ON refresh_token TYPE datetime;
DEFINE FIELD OVERWRITE grant ON refresh_token TYPE record<oauth_grant> REFERENCE ON DELETE CASCADE;
DEFINE FIELD OVERWRITE used_at ON refresh_token TYPE option<datetime>;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_refresh_token_grant ON refresh_token FIELDS grant;
