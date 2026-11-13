# Commits

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/). A fixed shape makes `git log --oneline` readable at a glance, shows what kind of change each commit is, and lets tools build a changelog or pick the next version from the history.

## Format

```
<type>(<scope>): <description>

<body>

<footer>
```

Only the first line is required. Most commits are just that line.

## Type

| Type | For |
|---|---|
| `feat` | a new capability |
| `fix` | a bug fix |
| `refactor` | a change in structure that doesn't change behaviour |
| `perf` | a change that only makes something faster or lighter |
| `test` | tests only |
| `docs` | documentation only, this book included |
| `build` | dependencies, the workspace, Docker, the toolchain |
| `chore` | anything else that doesn't touch the code's behaviour |
| `revert` | undoing an earlier commit, see [Reverting](#reverting) |

## Scope

The scope names the area the change is in, usually the crate without its `resysto-` prefix: `migrator`, `config`, `server`. Leave it out when a change spans several areas.

```
feat(mailer): retry when the provider rate-limits
docs: add the comments chapter
```

## Description

The description says what the commit does, in the imperative, lowercase, with no final period, in about 70 characters at most. It completes the sentence "this commit will ...":

```
good  fix(mailer): keep the reply-to address when forwarding
bad   fix(mailer): Fixed the reply-to address.
bad   fix: stuff
```

## Body

A body depends on the commit, and most commits don't need one: if the first line says it all, stop there. When there is one, it's a few lines at most, saying why the change was needed or what to watch out for, plus the issue it belongs to. No walls of text: the full explanation, the discussion and the alternatives live in the pull request, which the issue links to. Wrap it at 72 characters.

```
fix(mailer): keep the reply-to address when forwarding

The provider drops Reply-To on messages sent on behalf of another address.
Closes #42.
```

## Breaking changes

A change that breaks something outside the commit, such as a renamed setting, a removed endpoint, or a migration that needs manual steps, gets a `!` after the type or scope and a `BREAKING CHANGE:` footer that says what to do:

```
feat(config)!: read the SMTP password from a file

BREAKING CHANGE: replace RESYSTO_SMTP__PASSWORD with
RESYSTO_SMTP__PASSWORD_FILE pointing to a file with the password.
```

## One change per commit

A commit holds one logical change, together with its tests and the docs it makes stale, so it can be reviewed, reverted or cherry-picked on its own. If the description needs an "and", it's probably two commits.

## Reverting

A revert commit uses the `revert` type and quotes the header of the commit it undoes, so the log shows what went away without looking it up. The body keeps the hash of that commit, plus one line on why when it isn't obvious:

```
revert: 'fix(mailer): keep the reply-to address when forwarding'

This reverts commit 3f2a9c1.
The provider now keeps Reply-To itself, and setting it twice duplicates the header.
```

Reverting code doesn't revert the database. A commit that added a migration manifest or changed a schema file has already been applied to every database it reached, and reverting the files doesn't undo that: write a new manifest that moves the data and the schema where they need to be.
