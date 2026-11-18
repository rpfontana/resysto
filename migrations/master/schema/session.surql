-- the record key is the SHA-256 of the cookie's token, never the token
DEFINE TABLE OVERWRITE session SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE account ON session TYPE record<account> REFERENCE ON DELETE CASCADE;
DEFINE FIELD OVERWRITE created_at ON session TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE expires_at ON session TYPE datetime;
DEFINE FIELD OVERWRITE last_seen_at ON session TYPE datetime;
-- whether the login proved a second factor
DEFINE FIELD OVERWRITE mfa ON session TYPE bool;
DEFINE FIELD OVERWRITE user_agent ON session TYPE option<string>;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_session_account ON session FIELDS account;
