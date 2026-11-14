# Schema Files

A schema file describes one table as it should be: its fields, indexes, events and permissions. To add a field or an index, you change the file, and the migrator brings every database up to it.

## One file per table

Each lane keeps its files in `resysto/migrations/<lane>/schema/`, one file per table, named after the table:

```sql
-- migrations/tenant/schema/item.surql
DEFINE TABLE OVERWRITE item SCHEMAFULL;
DEFINE FIELD OVERWRITE name ON item TYPE string;
DEFINE FIELD OVERWRITE note ON item TYPE option<string>;
DEFINE INDEX OVERWRITE item_name ON item FIELDS name;
```

Every statement uses `OVERWRITE`, so the file can run again where the table already exists: each statement replaces the definition it names. The file is the final state of the table, so reading it tells you everything about the table, and you never have to replay a history of changes in your head.

## When a file runs

The migrator remembers the text of each file it has applied to a database. On every `migrate up` it applies again the files whose text has changed, and leaves the others alone. It records a file only after the file has run without errors, so a file that fails runs again on the next attempt.

## What a file can't do

A file only defines things. Taking a field or an index out of the file leaves it in the database, definition and stored values. Removing something takes a `remove` step in a manifest.

A file also can't say what happens to the rows already stored. When a change could make some of them invalid, the server refuses the file. On an empty table the same file goes through, so you find out only on a database with data. Changing Fields lists these cases.

## Indexes

Indexes go in their table's file, next to the fields they read.

Never write `CONCURRENTLY`. A concurrent build reports success right away, and if a unique index then finds a duplicate, the server rejects every later write to the table. The migrator refuses a file that contains the word. A normal build blocks the table until it ends, which is fine because nobody uses the table during a deploy.
