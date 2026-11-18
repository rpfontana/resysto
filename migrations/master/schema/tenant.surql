DEFINE TABLE OVERWRITE tenant SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE db_name ON tenant TYPE string;
-- the name in the URL: /t/{slug}/api/v1
DEFINE FIELD OVERWRITE slug ON tenant TYPE string;
DEFINE FIELD OVERWRITE state ON tenant
    TYPE 'provisioning' | 'ready' | 'suspended' | 'failed' | 'deleting' | 'deleted';
-- who has to sign in with a second factor
DEFINE FIELD OVERWRITE mfa_required_for ON tenant TYPE 'admins' | 'everyone' DEFAULT 'admins';
DEFINE INDEX OVERWRITE idx_tenant_db_name ON tenant FIELDS db_name UNIQUE;
DEFINE INDEX OVERWRITE idx_tenant_slug ON tenant FIELDS slug UNIQUE;
REMOVE INDEX IF EXISTS tenant_db_name ON tenant;
REMOVE INDEX IF EXISTS tenant_slug ON tenant;
