DEFINE TABLE OVERWRITE member_of TYPE RELATION IN account OUT tenant ENFORCED SCHEMAFULL
    PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE created_at ON member_of TYPE datetime DEFAULT time::now() READONLY;
-- NONE reaches the whole tenant; each organization listed brings its branch
DEFINE FIELD OVERWRITE org_scope ON member_of TYPE option<array<record<organization>>>;
DEFINE FIELD OVERWRITE roles ON member_of TYPE array<string> DEFAULT [];

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_member_of_in_out ON member_of FIELDS in, out UNIQUE;
