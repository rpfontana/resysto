DEFINE TABLE OVERWRITE tenant SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE db_name ON tenant TYPE string;
DEFINE FIELD OVERWRITE state ON tenant
    TYPE 'provisioning' | 'ready' | 'suspended' | 'failed' | 'deleting' | 'deleted';
DEFINE INDEX OVERWRITE tenant_db_name ON tenant FIELDS db_name UNIQUE;
