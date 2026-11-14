# Deploying

`resysto migrate up` runs before the new version of the app starts. It brings `__master` up to date first, then the tenant databases, a few at a time. A tenant that fails doesn't stop the others. At the end the command exits with 1 if any database failed, and the deploy stops there, before the app starts.

## Before a deploy

Run `resysto migrate plan` against production before you deploy. It shows what `migrate up` would do on each database, and changes nothing:

```
__master: up to date
t_acme:
  item.surql: refused: Cannot change field `note` of table `item`: … add MIGRATE EXISTING ROWS
t_globex:
  20261201093000-person-email.toml: would run
```

Each line under a database is a schema file or a manifest:

| Line | Meaning |
|---|---|
| `would be applied` | a changed schema file that the server accepts |
| `refused: …` | a changed schema file that the server refuses: it needs a manifest |
| `would run` | a manifest that hasn't run on this database |
| `would run again, it failed with: …` | a manifest that failed on the last run |

A database that `up` couldn't migrate at all, such as a tenant whose database is missing, gets `would fail:` and the reason. The command exits with 1 when `up` would fail somewhere.

To check a file, `plan` runs it in a transaction and then cancels the transaction, so a forgotten manifest shows up as the server's refusal. It has three limits:

- It doesn't try the files cited by a pending manifest, because they depend on what the manifest does first.
- It can't see a new `ASSERT` that stored rows break, because the server accepts it. That's what the `assert` step is for.
- A new index in a tried file is really built before the transaction is cancelled, which takes a few seconds on a large table.

## Reading the output of migrate up

`migrate up` prints what it applied to each database:

```
__master: up to date
t_acme:
  item.surql: applied
  20261201093000-person-email.toml: applied
t_initech: failed: manifest 20261201093000-person-email.toml, expand step 2 failed: …
Error: migrate up failed on 1 of the databases
```

After a failure, fix the cause and run `migrate up` again. What succeeded isn't repeated, and a failed manifest starts again from its first step.
