-- one row per record a write created, updated or deleted, written by each table's audit event
DEFINE TABLE OVERWRITE audit_change SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE after ON audit_change TYPE option<object> FLEXIBLE;
DEFINE FIELD OVERWRITE at ON audit_change TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE before ON audit_change TYPE option<object> FLEXIBLE;
-- NONE when the system wrote, such as a migration
DEFINE FIELD OVERWRITE by ON audit_change TYPE option<record<member>>;
-- the client_id of the agent's app, for writes through MCP
DEFINE FIELD OVERWRITE client ON audit_change TYPE option<string>;
DEFINE FIELD OVERWRITE event ON audit_change TYPE 'CREATE' | 'UPDATE' | 'DELETE';
-- the address the write came from, as the server saw it
DEFINE FIELD OVERWRITE ip ON audit_change TYPE option<string>;
DEFINE FIELD OVERWRITE record ON audit_change TYPE record;
DEFINE FIELD OVERWRITE request ON audit_change TYPE option<string>;
DEFINE FIELD OVERWRITE session ON audit_change TYPE option<string>;
DEFINE FIELD OVERWRITE user_agent ON audit_change TYPE option<string>;
DEFINE FIELD OVERWRITE via ON audit_change TYPE option<string>;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_audit_change_record ON audit_change FIELDS record;
