# MCP

Agents such as Claude work in resysto through MCP, the Model Context Protocol. They call tools, functions with a name, a description and an input schema, on behalf of the person who connected them. Resysto has one MCP endpoint, `/mcp`, for all its tenants. An agent acts for one member, in the tenants that member chose, with the member's permissions in each and never more. Every change it makes records both the member and the agent's app.

## The pieces

| Piece | Where it lives | Chapter |
|---|---|---|
| The endpoint: the protocol, which tools a caller sees, how a call runs | `crates/server/src/mcp.rs`, mounted at `/mcp` in `routes.rs` | How a Call Runs |
| The tools | `mcp.rs` next to `http.rs` in each bounded context, built with `resysto-mcp` | Writing Tools |
| The tokens: authorization server, consent, grants | `resysto-auth`: `oauth.rs`, `cimd.rs`, and `agent.rs` for the check on `/mcp` | Authorization |
| The trail | `audit_change` and `audit_event` | Database › Audit |

`rmcp` speaks the protocol, and only the server depends on it. Bounded contexts write their tools with the `Tool` type of `resysto-mcp`, which carries what an `rmcp` tool lacks: the effect (`Read`, `Write` or `Delete`). The effect decides who may call the tool and whether its database session can write. The server checks it before the tool runs, so a tool can't skip the check.

## Connecting a client

The URL is `RESYSTO_PUBLIC_URL` followed by `/mcp`: `http://localhost:3000/mcp` in development.

**Claude Code.** In development:

```sh
claude mcp add --transport http resysto-dev http://localhost:3000/mcp
```

In production, name the server after the deploy and use the deploy's URL. The name belongs to Claude Code alone. It tells servers apart and prefixes their tools (`mcp__resysto-dev__people_search`), and resysto never sees it. Keep it to letters, digits, `-` and `_`.

The first call gets a `401`, and Claude Code opens the browser at resysto. The person signs in, chooses the tenants the app may act in (all of them by default) and allows it. The browser then goes back to Claude Code, which keeps the tokens and renews them by itself. `/mcp` inside Claude Code shows the server's state and signs in again, for example to choose other tenants.

**claude.ai.** Desktop, mobile and Cowork share its connectors. Go to Settings › Connectors › Add custom connector and enter the same URL. Here Claude's servers make the calls, from `160.79.104.0/21`, so the URL must be public and https. A deploy reachable only from a private network works with Claude Code alone, which runs on the person's machine.

## Configuration

- **`RESYSTO_PUBLIC_URL`**: the URL clients use, without a path. It is https, or `http://localhost` or `http://127.0.0.1` in development. It must be exactly what people type, because three checks compare against it: the authorization server compares the token's resource with it, Claude compares the resource with the URL it was given, and the endpoint answers only requests whose `Host` is the URL's (`rmcp`'s guard against DNS rebinding). In development the value is `http://localhost:3000`, so `http://127.0.0.1:3000/mcp` doesn't work.
- **`RESYSTO_CLIENT_IP_SOURCE`**: where the caller's address comes from, for the audit.

Nothing else is needed. There are no signing keys, because tokens are opaque database rows, and apps don't register.

## The protocol

The endpoint speaks Streamable HTTP without sessions: no `Mcp-Session-Id`, every request stands alone, and it answers in JSON rather than with event streams. It accepts revision 2026-07-28 and earlier revisions such as 2025-11-25, which claude.ai uses:

- Under 2026-07-28 every request carries `Mcp-Method`, `tools/call` also carries `Mcp-Name`, and each request has the protocol version and client capabilities in its `_meta`.
- Under earlier revisions the client starts with `initialize`, which works without a session.

The endpoint offers tools only: no resources, prompts or sampling. The instructions it sends at `initialize` tell the agent how tenants work, that `tenants_list` gives them, and that `org_search` gives the organization ids other tools take.

## When something goes wrong

| What you see | Why |
|---|---|
| The sign-in fails right after the first `401` | The URL typed differs from `RESYSTO_PUBLIC_URL`, such as `127.0.0.1` for `localhost`. Claude refuses metadata whose resource is not the URL it was given. |
| `403 Forbidden: Host header is not allowed` | The request's `Host` is not the public URL's: another host name, or a proxy that rewrites `Host`. |
| The browser shows `redirect_uri is not one of the redirect URIs of …` | The app asked to go back to an address its metadata document doesn't list. |
| The browser shows `client_id points to a private network` | The app's metadata document is on a private address. It must be on a public https URL. |
| The app gets `error=invalid_target` | It asked for a token for something other than `{public}/mcp`. |
| A tool answers "you can't act in the tenant …" | The tenant is not in the grant, or the account is no longer a member. Sign the app in again and choose the tenant. |
| A tool answers that a tenant asks for a second factor | The consent was given without one. Sign in with the authenticator app, then sign the app in again. |
| `tools/list` shows only `tenants_list` | No tenant of the grant lets the member in any more, or the member's roles allow no tool. |
