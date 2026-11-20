# Schema Files

A schema file describes one table as it should be: its fields, indexes, events and permissions. To add a field or an index, you change the file, and the migrator brings every database up to it.

## One file per table

Each lane keeps its files in `resysto/migrations/<lane>/schema/`, one file per table, named after the table:

```sql
-- migrations/tenant/schema/item.surql
DEFINE TABLE OVERWRITE item SCHEMAFULL;

DEFINE FIELD OVERWRITE name ON item TYPE string;
DEFINE FIELD OVERWRITE note ON item TYPE option<string>;

DEFINE INDEX OVERWRITE idx_item_name ON item FIELDS name;
```

Every statement uses `OVERWRITE`, so the file can run again where the table already exists: each statement replaces the definition it names. The file is the final state of the table, so reading it tells you everything about the table, and you never have to replay a history of changes in your head.

A definition that belongs to no table, such as the database's `DEFINE ACCESS`, goes in a file of its own, named after what it defines: `access.surql`.

## Inside a file

Every file has the layout SurrealDB gives a table's definition (`INFO FOR TABLE`, or Show definition in Surrealist), so you can hold the file next to what the database holds:

1. `DEFINE TABLE`, with its permissions. When the record key isn't a UUID v7, a comment above it says what it is.
2. The fields, under a `FIELDS` header.
3. The indexes, under an `INDEXES` header.
4. The events, under an `EVENTS` header.

Inside a section, statements go in alphabetical order of their names, as the database lists them: a nested field comes right after its parent (`totp`, then `totp.secret`), and `idx_<table>` before the other indexes. A section with nothing in it has no header. A comment that explains a statement sits right above it, and moves with it. A statement that doesn't fit on one line continues on lines indented by four spaces.

The definition SurrealDB prints also spells out defaults, such as `TYPE NORMAL` on a table and `PERMISSIONS FULL` on a field. The file leaves them out.

```sql
-- migrations/tenant/schema/people/person.surql, with some fields left out
DEFINE TABLE OVERWRITE person SCHEMAFULL PERMISSIONS
    FOR select WHERE $token.org_all = true
        OR organization_ids.map(|$o| <string> $o) CONTAINSANY $token.org_scope
    FOR create, update, delete WHERE $token.write = true AND ($token.org_all = true
        OR organization_ids.map(|$o| <string> $o) ALLINSIDE $token.org_scope);

-- ------------------------------
-- FIELDS
-- ------------------------------

DEFINE FIELD OVERWRITE created_at ON person TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD OVERWRITE created_by ON person TYPE option<record<member>> DEFAULT $auth READONLY;
DEFINE FIELD OVERWRITE email ON person TYPE option<string>;
DEFINE FIELD OVERWRITE first_name ON person TYPE string;
DEFINE FIELD OVERWRITE id ON person;
DEFINE FIELD OVERWRITE last_name ON person TYPE string;
DEFINE FIELD OVERWRITE organization_ids ON person TYPE array<record<organization>>
    REFERENCE ON DELETE REJECT ASSERT array::len($value) > 0;
DEFINE FIELD OVERWRITE updated_at ON person TYPE datetime VALUE time::now();
DEFINE FIELD OVERWRITE updated_by ON person TYPE option<record<member>> VALUE $auth;
DEFINE FIELD OVERWRITE version ON person TYPE int VALUE ($before ?? 0) + 1;

-- ------------------------------
-- INDEXES
-- ------------------------------

DEFINE INDEX OVERWRITE idx_person ON person FIELDS id;
```

## When a file runs

The migrator remembers the text of each file it has applied to a database. On every `resysto-migrator up` it applies again the files whose text has changed, and leaves the others alone. It records a file only after the file has run without errors, so a file that fails runs again on the next attempt.

## What a file can't do

A file only defines things. Taking a field or an index out of the file leaves it in the database, definition and stored values. Removing something takes a `remove` step in a manifest.

A file also can't say what happens to the rows already stored. When a change could make some of them invalid, the server refuses the file. On an empty table the same file goes through, so you find out only on a database with data. Changing Fields lists these cases.

## Indexes

Indexes go in their table's file, under `INDEXES`. An index is named `idx_<table>_<fields>`, with the fields it reads joined by `_`: `idx_person_email`, `idx_member_of_in_out`. A unique index has the same form, since `UNIQUE` is in its definition. The index on `id` is just `idx_<table>`.

Every table with a list has `idx_<table>`. Lists read the newest rows first, below the cursor, and when a select permission applies to the caller, SurrealDB 4 doesn't turn `id < $before` into a range over the record keys: without the index, each page reads every row newer than the cursor. A schemafull table takes an index on `id` only once `id` is a field, so the file also has `DEFINE FIELD OVERWRITE id ON <table>;`, with no type. The plan test in the table's `store.rs` fails when the index is missing.

To rename an index, define it under the new name in the file, and remove the old name with a `remove` step in a manifest.

Never write `CONCURRENTLY`. A concurrent build reports success right away, and if a unique index then finds a duplicate, the server rejects every later write to the table. The migrator refuses a file that contains the word. A normal build blocks the table until it ends, which is fine because nobody uses the table during a deploy.
