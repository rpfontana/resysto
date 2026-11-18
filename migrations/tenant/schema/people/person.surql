DEFINE TABLE OVERWRITE person SCHEMAFULL PERMISSIONS
    FOR select WHERE $token.org_all = true
        OR organization_ids.map(|$o| <string> $o) CONTAINSANY $token.org_scope
    FOR create, update, delete WHERE $token.write = true AND ($token.org_all = true
        OR organization_ids.map(|$o| <string> $o) ALLINSIDE $token.org_scope);

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE created_at ON person TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE department ON person TYPE option<string>;
DEFINE FIELD OVERWRITE email ON person TYPE option<string>;
DEFINE FIELD OVERWRITE first_name ON person TYPE string;
DEFINE FIELD OVERWRITE id ON person;
DEFINE FIELD OVERWRITE job_title ON person TYPE option<string>;
DEFINE FIELD OVERWRITE last_name ON person TYPE string;
DEFINE FIELD OVERWRITE organization_ids ON person TYPE array<record<organization>>
    REFERENCE ON DELETE REJECT ASSERT array::len($value) > 0;
DEFINE FIELD OVERWRITE phone ON person TYPE option<string>;
DEFINE FIELD OVERWRITE updated_at ON person TYPE datetime VALUE time::now();
DEFINE FIELD OVERWRITE version ON person TYPE int VALUE ($before ?? 0) + 1;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_person ON person FIELDS id;
