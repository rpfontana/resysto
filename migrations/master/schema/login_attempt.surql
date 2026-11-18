-- the record key is the SHA-256 of the email tried, whether an account has it or not
DEFINE TABLE OVERWRITE login_attempt SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE failures ON login_attempt TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE locked_until ON login_attempt TYPE option<datetime>;
DEFINE FIELD OVERWRITE updated_at ON login_attempt TYPE datetime VALUE time::now();
