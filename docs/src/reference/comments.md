# Comments

The code says what it does. A comment says why it does it that way. We write few comments, and each one has to earn its place: a comment that repeats the code has to be kept in step with it, and the day the code changes and the comment doesn't, it starts lying.

## Explain why, not what

A comment is worth writing when someone reading the code would otherwise "fix" it and break something:

- an external constraint, such as an API limit or a database behaviour;
- a choice that looks wrong but isn't;
- an obvious alternative that was tried and rejected;
- an invariant the types can't express.

```rust
// bad: says what the next line does
// wait one minute before retrying
let backoff = Duration::from_secs(60);

// good: says why the value looks odd
// the provider counts requests per minute, so a shorter wait only burns attempts
let backoff = Duration::from_secs(60);
```

If you need a comment to explain *what* a block does, the code is asking for a better name or a function of its own:

```rust
// bad
// keep only the users that can log in
let users: Vec<_> = users.into_iter().filter(|u| u.active && !u.locked).collect();

// good
let users: Vec<_> = users.into_iter().filter(User::can_log_in).collect();
```

## No tickets, dates or links

A comment states the fact, not where it was discussed:

```rust
// bad
// see #188 and docs/infra.md, broken on 2026-10-19
// good
// the API returns at most 500 patches per page, whatever `limit` says
```

Links break, trackers change, and the reader shouldn't need to open another page to understand one line. The history of a change belongs in the commit message and the pull request, where `git blame` finds it.

## Format

Line comments start lowercase and have no final period. One or two lines; a reason that needs a paragraph belongs in the commit message or in this book.

```rust
// the vendor rejects an empty array, so the field is left out instead
if tags.is_empty() {
    body.remove("tags");
}
```

## Doc comments

A `///` on a public item tells the caller what the signature can't: a precondition, a side effect, an invariant, the shape an input must have. Doc comments are useful, so write them, but in the right amount: as much as the caller needs and no more. Usually that's one sentence; sometimes a second line, or a short example when it saves the caller a trip into the code.

```rust
/// Sends the message; the provider may deliver it twice, so the body must be idempotent
pub async fn send(message: Message) -> Result<(), SendError>
```

```rust
/// Renders a template, filling each `{{name}}` placeholder from `values`
///
/// ```text
/// Hello {{first_name}}, your report for {{month}} is ready
/// ```
pub fn render(template: &str, values: &Values) -> Result<String, RenderError>
```

- Don't restate the name or the type: `/// Returns the user` on `fn user()` is noise.
- If a caller can misuse the function without knowing something, that something goes in the doc.
- No `# Errors` sections: the error type is in the signature, and each variant carries its own message.
- A one-line doc is capitalized and has no final period.
- Private items rarely need a doc comment. If something about them is surprising, a `//` explaining why is enough.

## Module docs

A `//!` at the top of a file says in a few words what the module is for, or isn't there at all:

```rust
//! Outbound email
```

## Tests

Test comments follow the same rule: explain why the setup looks the way it does, not the steps a test obviously takes.

```rust
// 1,200 rows, so the batches are 500, 500 and 200
let rows = seed(1_200).await;
```
