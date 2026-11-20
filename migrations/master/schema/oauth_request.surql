-- an authorization waiting for consent, then for the code exchange; its key is in the consent URL
DEFINE TABLE OVERWRITE oauth_request SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE client ON oauth_request TYPE string;
DEFINE FIELD OVERWRITE client_name ON oauth_request TYPE string;
-- the SHA-256 of the authorization code
DEFINE FIELD OVERWRITE code ON oauth_request TYPE option<string>;
DEFINE FIELD OVERWRITE code_challenge ON oauth_request TYPE string;
DEFINE FIELD OVERWRITE created_at ON oauth_request TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE expires_at ON oauth_request TYPE datetime;
DEFINE FIELD OVERWRITE grant ON oauth_request TYPE option<record<oauth_grant>>
    REFERENCE ON DELETE CASCADE;
DEFINE FIELD OVERWRITE redirect_uri ON oauth_request TYPE string;
DEFINE FIELD OVERWRITE state ON oauth_request TYPE option<string>;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_oauth_request_code ON oauth_request FIELDS code;
