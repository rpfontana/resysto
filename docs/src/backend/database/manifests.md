# Manifests

Some changes aren't a final state: copying values from an old field to a new one, converting stored values, dropping a field. A schema file can't express them, because it says how the table should look, not how to get there from the data already stored. They go in a manifest, a TOML file that lists steps and runs once on each database.

## An example

A manifest lives in `resysto/migrations/<lane>/`, next to the `schema/` folder, and is named `<UTC YYYYMMDDHHMMSS>-<title>.toml`. The timestamp sets the order in which manifests run, so take it from the clock when you create the file.

This one renames a field of `person` from `mail` to `email`:

```toml
# migrations/tenant/20261201093000-person-email.toml
title = "person.mail renamed to email"

[[expand]]
schema = "person.surql"

[[expand]]
data = { sql = """
    LET $batch = (SELECT VALUE id FROM type::record('person', $range)
                  WHERE email = NONE LIMIT $limit);
    UPDATE $batch SET email = mail RETURN NONE;
    RETURN $batch;
""", batch = 500 }

[[contract]]
remove = { table = "person", field = "mail" }
```

In the same commit, `person.surql` defines `email` as `option<string>` and no longer mentions `mail`. The three steps run in order:

1. The `schema` step applies `person.surql`. It adds `email`, and `mail` stays in the database, because a file doesn't remove anything.
2. The `data` step copies `mail` into `email`, 500 rows at a time.
3. The `remove` step deletes `mail` and its values.

To make `email` required afterwards, see Changing Fields.

## Steps

| Step | What it does |
|---|---|
| `schema = "person.surql"` | applies a schema file at this point |
| `data = { sql = "…", batch = 500 }` | runs a query in batches, until a batch comes back smaller than the others |
| `assert = "RETURN …"` | stops the manifest unless the query returns `true` |
| `remove = { table = "…", field = "…" }` | removes a table, or one of its fields, indexes or events |
| `migrate = "DEFINE FIELD … MIGRATE EXISTING ROWS …"` | converts the values a field already holds |

A `data` query reads two parameters: `$range` covers the ids after the last batch, and `$limit` is the batch size. Its last statement returns the ids of the batch, so the migrator knows where the next one starts. Batches keep each transaction small, however large the table.

An `assert` checks the data before a step that relies on it, for example that every value can be converted. Changing Fields shows it at work, along with `migrate`.

## Expand and contract

Steps go in two lists. `expand` steps add the new shape next to the old one and fill it. `contract` steps take the old shape away. The migrator runs the `expand` steps, then the `contract` steps, in the same run of `resysto-migrator up`.

The split marks the point after which data is lost. `remove` is allowed only in `contract`, so nothing is deleted before the new data is in place.

## How a manifest runs

- It runs once on each existing database, in timestamp order. The migrator records it as done once every step has succeeded.
- If a step fails, the manifest starts again from its first step on the next run. Write every step so that running it twice does no harm. In the example, `WHERE email = NONE` skips the rows already copied.
- Don't edit a manifest after it has run: the migrator refuses one whose text changed. Write a new manifest instead. You can fix a manifest that failed, as long as it hasn't succeeded on any database, because a database where it succeeded refuses the edited text.
- The migrator refuses a new manifest that is older than one that has already run. This happens when a branch is merged long after its manifest was created. Give the file a new timestamp.
- A schema file cited by a manifest that hasn't run yet waits for it: the manifest's `schema` step applies the file, after the steps that prepare the data.

A new database, such as the one of a tenant being provisioned, runs no manifest. It gets every schema file, which already describes the final state, and records every manifest as done, because there are no rows to move. So a manifest only changes data that is already there: never use one to create rows that every database needs, because a new database skips it.
