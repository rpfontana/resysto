# Changing Fields

At compatibility level 4, SurrealDB checks a field change against the rows a `SCHEMAFULL` table already holds. If the stored values might not fit the new definition, it refuses the change, and the schema file fails. On an empty table, as in development, the same file goes through, so a missing manifest shows up only on a database with data.

| Change | What it needs |
|---|---|
| widening a type, adding an optional field | only the file |
| making a field required, narrowing its type | a manifest with a `migrate` step |
| adding a required field | a manifest with a `migrate` step |
| adding an `ASSERT` | a manifest with an `assert` step |
| removing a field | a manifest with a `remove` step |

## Widening

Widening a type, such as `option<int>` to `option<int | string>`, or any numeric type to `number`, only changes the file, because every stored value still fits. The same goes for a new optional field.

## Making a field required or narrowing its type

Changing `option<string>` to `string`, or `int` to `string`, is refused with the hint `add MIGRATE EXISTING ROWS`: some stored values might be `NONE`, or numbers. A `migrate` step tells the server what they become:

```toml
title = "item.note required"

[[contract]]
migrate = """
    DEFINE FIELD OVERWRITE note ON item TYPE string
        MIGRATE EXISTING ROWS AS note_required USING $value ?? '' FALLBACK ''
"""

[[contract]]
schema = "item.surql"
```

`$value` is the stored value, `USING` computes the new value from it, and `FALLBACK` is stored when `USING` fails. `AS` names the migration: the server records the name on the table, and skips the statement when it runs again unchanged. The server converts each row the first time it's read, and rewrites the table in the background.

The `schema` step then applies the file with the final definition.

Write `MIGRATE EXISTING ROWS` only in the manifest, never in the `.surql` file. The file runs again whenever its text changes, and on a later run the server would refuse the clause: for example once the field has an index, or once its type has changed under the same migration name.

### When an index reads the field

The server refuses to migrate a field that an index reads. Remove the index first, and the `schema` step puts it back:

```toml
[[contract]]
remove = { table = "person", index = "idx_person_email" }

[[contract]]
migrate = "DEFINE FIELD OVERWRITE email ON person TYPE string MIGRATE EXISTING ROWS AS email_required USING $value ?? '' FALLBACK ''"

[[contract]]
schema = "person.surql"
```

## Adding a required field

A new field whose type doesn't accept `NONE` is refused too, even with a `DEFAULT`, because the rows already stored hold nothing in it. Give them a value with a `migrate` step, then cite the file:

```toml
title = "item.stock"

[[expand]]
migrate = "DEFINE FIELD OVERWRITE stock ON item TYPE int MIGRATE EXISTING ROWS AS add_stock USING 0"

[[expand]]
schema = "item.surql"
```

The `schema` step is needed even though the `migrate` step already defines the field. Citing the file makes it wait for the manifest. Without that, the migrator would apply `item.surql` before the manifest, and the server would refuse it.

## Adding an ASSERT

The server accepts a new `ASSERT` even when stored rows break it. Nothing fails during the deploy. Later, every update of one of those rows fails, in production, in front of a user. So check the rows with an `assert` step before the `schema` step:

```toml
title = "person.email not empty"

[[expand]]
assert = "RETURN count(SELECT VALUE id FROM person WHERE string::len(email) = 0 LIMIT 1) = 0"

[[expand]]
schema = "person.surql"
```

If some rows break the rule, the manifest stops before the file runs, `resysto-migrator up` fails, and the new version doesn't start. It stops again on every attempt until the rows are fixed, by hand or with a `data` step in front of the `assert`.

The server does count the violations once, in the background, but that count is undocumented and never refreshed, so the migrator can't rely on it.

## Removing a field

Taking a field out of the file leaves it in the database, definition and values. Add a `remove` step in `contract`. At level 4 it deletes the stored values along with the definition, and nothing brings them back, so move the data you need first, as the rename in Manifests does.
