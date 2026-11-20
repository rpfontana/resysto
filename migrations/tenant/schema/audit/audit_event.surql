-- what happened in the tenant without changing a record, written by the server
DEFINE TABLE OVERWRITE audit_event SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE action ON audit_event TYPE option<'read' | 'write' | 'delete'>;
DEFINE FIELD OVERWRITE at ON audit_event TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE by ON audit_event TYPE option<record<member>>;
DEFINE FIELD OVERWRITE event ON audit_event TYPE 'refused';
DEFINE FIELD OVERWRITE ip ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE request ON audit_event TYPE option<string>;
-- the REST resource of the request, such as people
DEFINE FIELD OVERWRITE resource ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE session ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE user_agent ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE via ON audit_event TYPE option<string>;
