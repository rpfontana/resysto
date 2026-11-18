-- a login whose password was right, waiting for its second factor; the record key is the
-- SHA-256 of the cookie's token
DEFINE TABLE OVERWRITE login_challenge SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE account ON login_challenge TYPE record<account> REFERENCE ON DELETE CASCADE;
DEFINE FIELD OVERWRITE expires_at ON login_challenge TYPE datetime;
DEFINE FIELD OVERWRITE user_agent ON login_challenge TYPE option<string>;
