# How a Call Runs

Every request to `/mcp` goes through the same steps. The code is in `crates/server/src/mcp.rs` and in `agent.rs` of `resysto-auth`.

## The token

The middleware `require_agent` reads `Authorization: Bearer`, looks up the token's SHA-256 in `access_token` and follows it to its grant. The token and its grant must not have expired, and the account must be active. When no token is sent, or the token isn't live, the answer is `401` with `WWW-Authenticate: Bearer resource_metadata="…"`. That header tells the client where to get a token, and it adds `error="invalid_token"` when a token was sent.

A live token puts an `Agent` in the request. It holds the account, the app's `client_id`, whether the consent proved a second factor, the tenants of the grant, and the request's id, address and `User-Agent` for the audit. The request itself names no tenant: each call does.

## Which tools an agent sees

A tenant is within the agent's reach when four things hold:

- it is in the grant;
- it is ready;
- the account is still a member;
- if it asks this member for a second factor, the consent proved one.

`tools/list` starts with `tenants_list`, then lists every tool the member's roles allow in at least one tenant within reach. The decision is Cedar's, exactly as for a REST request: the tool's effect is the verb, and the name it is mounted under is the resource. A `Read` tool is marked `readOnlyHint` and a `Delete` tool `destructiveHint`, which clients use to ask the person for confirmation.

## The tenant of a call

The server adds a `tenant` parameter, the tenant's slug, to the schema of every mounted tool. The agent may leave it out when only one tenant is within reach; otherwise the refusal lists the tenants to choose from. `tenants_list`, written in the server, gives the tenants within reach and the member's roles in each:

```json
{ "tenants": [
    { "slug": "acme", "roles": ["tenant/admin"] },
    { "slug": "beta", "roles": ["tenant/reader"] }
] }
```

The server takes `tenant` out of the arguments before the tool sees them, so a tool is written as if there were a single tenant.

## A call, step by step

1. **The tool.** An unknown name is a protocol error, `invalid params`.
2. **The tenant.** It is the one the `tenant` parameter names, or the only one within reach.
3. **Entering it.** `Agent::enter` reads the membership again: the tenant must be in the grant and ready, the account a member, and the second factor proved if the tenant asks for it. It returns the member as it acts there, with its roles and organizations, and the actor the database records.
4. **The decision.** Cedar decides on the roles in that tenant, the tool's effect and its resource. A refusal is recorded in that tenant's `audit_event` (`refused`, `via: mcp`, the client), and the agent is told not to try again.
5. **The session.** The member's organizations widen to their branches, as for a REST request. A record session opens on that tenant's database, with `write = false` for a `Read` tool. The database then refuses whatever the roles and organizations don't allow, even when the tool tries it.
6. **The tool.** It runs with that session and the remaining arguments.
   - Its answer goes back as structured content.
   - A `ToolError::Refused` goes back as an error result carrying its message.
   - A `ToolError::Internal` is logged, and the agent learns only that the tool failed.

All of this runs again on every call. A changed role, a removed membership or a suspended tenant takes effect at the next call, with no token to revoke.

## Isolation

A token reaches several tenants, but a call never reaches more than one: it opens a session on one tenant's database, after the checks on that tenant. The browser's session cookie works the same way: it reaches every tenant of its account, and each request names one in its path. A stolen token exposes the tenants of its grant, until it expires after an hour or is revoked.

## What the audit records

| Where | What |
|---|---|
| The tenant's `audit_change` | Every record the agent changes, with `via = 'mcp'`. `client` holds the app's `client_id` and `session` the key of the access token. `request` holds the request's id, and the address and `User-Agent` of the call are there too. |
| The tenant's `audit_event` | The calls the policies refused. |
| The master's `audit_event` | Consents, and the tokens issued, reused and revoked; see Authorization. |

`session` changes every hour as the app renews its access token, while `client` stays the same.
