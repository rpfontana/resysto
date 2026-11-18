# Database

Resysto keeps its data in SurrealDB 4 on the SurrealKV engine. One namespace holds a master database, `__master`, with the list of tenants, and one database per tenant, named `t_<slug>_<hex8>`, with that tenant's data.

The schema of every database comes from files in the repository, and `resysto-migrator up` brings each database up to them. It runs at deploy time, before the app starts, so nobody reads or writes while the schema changes. A migration never has to cope with live traffic, and a slow step only makes the deploy slower.

The files are split into two lanes under `resysto/migrations/`: `master` for `__master`, and `tenant` for every tenant database. A new tenant database starts from the same tenant files, so it ends up with the same schema as the others.

Our databases run at compatibility level 4, the level of every database created on SurrealDB 4. At this level the server refuses a schema change that the rows already stored might not satisfy, so changing a table that holds data takes more than writing its new definition. The chapters of this section explain how:

- **Schema Files**: how a table's definition is written, and when the migrator applies it.
- **Manifests**: the changes a definition can't express, such as copying values from one field to another.
- **Changing Fields**: the field changes the server refuses on a table with rows, and the manifest each one needs.
- **Deploying**: checking a change against production with `resysto-migrator plan`, and reading what `up` prints.
