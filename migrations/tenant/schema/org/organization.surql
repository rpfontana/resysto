DEFINE TABLE OVERWRITE organization SCHEMAFULL PERMISSIONS
    FOR select WHERE $token.org_all = true OR <string> id IN $token.org_scope
    FOR create, update, delete WHERE $token.write = true AND $token.org_all = true;

DEFINE FIELD OVERWRITE name ON organization TYPE string;
DEFINE FIELD OVERWRITE parent ON organization TYPE option<record<organization>>;
-- the ancestors from the root down to the parent
DEFINE FIELD OVERWRITE path ON organization TYPE array<record<organization>>;
DEFINE FIELD OVERWRITE active ON organization TYPE bool DEFAULT true;
DEFINE FIELD OVERWRITE org_type ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE description ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE notes ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE email ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE phone ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE website ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE address ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE city ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE postal_code ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE country ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE vat_number ON organization TYPE option<string>;
DEFINE FIELD OVERWRITE industry ON organization TYPE option<
    'technology' | 'finance' | 'healthcare' | 'manufacturing' | 'retail' | 'education'
    | 'government' | 'energy' | 'transportation' | 'telecommunications' | 'real_estate'
    | 'agriculture' | 'entertainment' | 'hospitality' | 'other'
>;
DEFINE FIELD OVERWRITE annual_income ON organization TYPE option<int>;
DEFINE FIELD OVERWRITE employee_count ON organization TYPE option<int>;
DEFINE FIELD OVERWRITE created_at ON organization TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE updated_at ON organization TYPE datetime VALUE time::now();

DEFINE INDEX OVERWRITE organization_parent ON organization FIELDS parent;
