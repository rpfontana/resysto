# Writing Tools

Each bounded context offers its own tools, the way it offers its routes. This chapter is about writing them.

## Tools are not endpoints

An agent is not a program calling an API. It reads every answer into a limited context, it guesses what a field means from its name, and it pays for every call in time and tokens. Wrapping each REST endpoint in a tool gives it the wrong shape. Anthropic's guidance for tools (["Writing effective tools for agents"](https://www.anthropic.com/engineering/writing-tools-for-agents)) comes down to these rules, which every tool here follows:

- **Few tools, shaped around tasks.** A tool covers something a person asks for, and may take several steps to do it. `people_add` finds the organizations by name and adds the person, where the API needs a lookup and a create. Tools that overlap make the agent hesitate.
- **Search, don't list.** `people_search` takes part of a name. A tool that returns everything spends the agent's context on rows it doesn't need.
- **Readable answers.** Return names where the API returns ids: `"Acme › Italia › Milano"`, not three uuids. Keep the ids the agent needs for the next call, and nothing else unless it asks. List tools take `response_format`: `concise` by default, `detailed` for every field.
- **Small pages.** Lists return 20 rows unless asked for more, and never more than 50 (`resysto_mcp::limit`), with a `next_cursor`. Claude Code cuts a tool's answer at 25,000 tokens.
- **Errors that say what to do.** "`"Milano"` names 2 organizations: Acme › Milano (…), Beta › Milano (…); pass the id of the one you mean" lets the agent recover on its own. A bare `not_found` doesn't.
- **Descriptions written for a new colleague.** Say what the tool is for, when to call it, which tool gives its inputs, and the words of the domain. Parameter names leave no doubt: `organization_id`, not `organization`.
- **A prefix per bounded context:** `people_search`, `org_overview`.

## Writing a tool

The tools of an entity live in `mcp.rs`, next to `http.rs`, and the module exports them:

```rust
// crates/core/people/src/person/mcp.rs
pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("people_search", Effect::Read, SEARCH, search),
        Tool::new("people_add", Effect::Write, ADD, add),
    ]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    /// Part of a first name, last name or email; leave it out to list everyone
    text: Option<String>,
    // ...
}

async fn search(db: Surreal<Any>, input: Search) -> Result<Found, ToolError> {
    // ...
}
```

The input is a struct that derives `Deserialize` and `JsonSchema`, with `deny_unknown_fields`. The doc comment of each field becomes its description in the schema the agent reads. An input never has a field named `tenant`, because the server adds that parameter itself and takes it out before the tool runs (How a Call Runs); a test in `routes.rs` checks this.

The answer is any `Serialize` type, and the server sends it as structured content. The function takes ownership of the session the server opened for the call, on one tenant's database, and calls the entity's operations in `ops`, which keep their rules for every caller. Lookups that only the tools need, such as finding organizations by name, are queries in `store.rs`.

Then the server mounts the tools in `routes.rs`, under the resource name their routes have:

```rust
let mounted = [
    ("organizations", resysto_org::organization::tools()),
    ("people", resysto_people::person::tools()),
];
```

## What a tool may do

A tool declares its `Effect`: `Read`, `Write` or `Delete`. It is the only thing a tool declares, because a tool call has no HTTP method to say what it does. The effect decides three things:

- **Who sees and calls the tool.** The effect is the verb of the Cedar decision, and the mounted name is the resource, exactly as for a REST request.
- **What the session can do.** A `Read` tool runs on a record session with `write = false`, so the database refuses its writes even when the code tries one. The organization scope applies as for any request.
- **What the client is told.** `Read` is sent as `readOnlyHint` and `Delete` as `destructiveHint`. Clients use them, for example, to ask for confirmation.

## Errors

A tool returns `ToolError::Refused` with a message for the agent when the call can't go through and the agent can do something about it: a wrong input, an ambiguous name, a record that changed since it was read. Map each error of the operations to such a message, as `refusal` does in `people`. A validation error of an input struct becomes one line per field.

`ToolError::Internal` is for faults of the server. It is logged with its details, and the agent only learns that the tool failed.

## Testing

Call the tool's function on a record session of a fresh tenant, with the input built from JSON as the agent would send it. Assert on what matters: the names it resolved, the refusal and its candidates, the version check. Who may call what, which tenant a call reaches, and that a read can't write are tested once, over HTTP, in the server's `mcp.rs`.
