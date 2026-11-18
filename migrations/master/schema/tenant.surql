DEFINE TABLE OVERWRITE tenant SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE db_name ON tenant TYPE string;
-- who has to sign in with a second factor
DEFINE FIELD OVERWRITE mfa_required_for ON tenant TYPE 'admins' | 'everyone' DEFAULT 'admins';
-- the name in the URL: /t/{slug}/api/v1
DEFINE FIELD OVERWRITE slug ON tenant TYPE string;
DEFINE FIELD OVERWRITE state ON tenant
    TYPE 'provisioning' | 'ready' | 'suspended' | 'failed' | 'deleting' | 'deleted';

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_tenant_db_name ON tenant FIELDS db_name UNIQUE;
DEFINE INDEX OVERWRITE idx_tenant_slug ON tenant FIELDS slug UNIQUE;
