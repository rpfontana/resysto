-- the record key is the uuid of the member's account in the master
DEFINE TABLE OVERWRITE member SCHEMAFULL PERMISSIONS
    FOR select FULL;

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE email ON member TYPE string;
DEFINE FIELD OVERWRITE updated_at ON member TYPE datetime VALUE time::now();
