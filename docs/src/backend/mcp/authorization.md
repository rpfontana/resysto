# Authorization

An agent calls `/mcp` with an access token, and resysto issues that token itself. The server is the OAuth 2.1 authorization server the MCP authorization spec asks for, running in the same process and on the same database. The member signs in to resysto and allows the agent's app. The app never sees the password, and gets a token for the tenants the member chose.

## The endpoints

| Endpoint | What it does |
|---|---|
| `GET /.well-known/oauth-protected-resource/mcp` | Describes the resource, `{public}/mcp`, and names its authorization server, `{public}` (RFC 9728). |
| `GET /.well-known/oauth-authorization-server` | Lists the endpoints and what they support (RFC 8414). |
| `GET /oauth/authorize` | Starts an authorization and sends the browser to the consent page. |
| `POST /oauth/token` | Exchanges a code or a refresh token for tokens. |
| `POST /oauth/revoke` | Revokes a token (RFC 7009). |
| `GET /api/v1/auth/oauth/requests/{id}` | Gives the consent page what to show. |
| `POST /api/v1/auth/oauth/requests/{id}/approve` | Records the member's yes, with the tenants they chose. |
| `POST /api/v1/auth/oauth/requests/{id}/deny` | Records the member's no. |

The last three serve the SPA and take the session cookie, like every route under `/api/v1/auth`.

## The flow

1. The app calls `/mcp` without a token. The answer is `401` with `WWW-Authenticate: Bearer resource_metadata="{public}/.well-known/oauth-protected-resource/mcp"`.
2. The app reads the two metadata documents.
3. The app opens the browser at `/oauth/authorize` with its `client_id`, a `redirect_uri`, a PKCE `code_challenge` with method `S256`, its `state` and `resource={public}/mcp` (RFC 8707).
4. The server fetches the app's metadata document and checks the request. It keeps the request in `oauth_request` and sends the browser to the SPA's consent page, `/oauth/consent?request={id}`.
5. The consent page signs the member in if needed. It shows the app, where the answer goes, and the member's tenants, all of them chosen by default.
6. On approval the server checks two things: the account is a member of each tenant chosen, and the session proved the second factor any of those tenants asks for. It creates the `oauth_grant` and sends the browser back to the app with a `code`, the `state` and `iss` (RFC 9207).
7. The app exchanges the code and its PKCE verifier at `/oauth/token` for an access token and a refresh token. It then calls `/mcp` with `Authorization: Bearer …`.

A tenant the member joins later stays out of the grant until the member allows the app again.

## The consent API

`GET /api/v1/auth/oauth/requests/{id}` answers:

```json
{ "data": {
    "client_name": "Claude Code",
    "client_id": "https://claude.ai/oauth/claude-code-client-metadata",
    "redirect_host": "localhost",
    "loopback": true,
    "tenants": [
        { "slug": "acme", "roles": ["tenant/admin"], "second_factor_needed": false }
    ]
} }
```

What the fields mean:
- `loopback`: the answer goes to a program on the member's own machine, as with Claude Code.
- `second_factor_needed`: the tenant asks this member for a second factor that the session hasn't proved.

`approve` takes the slugs chosen, `{ "tenants": ["acme"] }`, and both `approve` and `deny` answer `{ "data": { "redirect": "…" } }`, where the page sends the browser next. The errors:

| Status | Meaning |
|---|---|
| `400` | The list of tenants is empty. |
| `401` | No session. |
| `404` | The request is unknown, expired or already answered. |
| `422` | The list names a tenant the account is not a member of. |
| `403 mfa_required` | A chosen tenant asks for a second factor that the session hasn't proved. |

## Apps identify themselves by a URL

An app doesn't register. Its `client_id` is an https URL serving a JSON document, its Client ID Metadata Document, with its name and redirect URIs. Claude Code's is `https://claude.ai/oauth/claude-code-client-metadata`. The server metadata advertises `client_id_metadata_document_supported` and `token_endpoint_auth_methods_supported: ["none"]`; Claude uses the document only when both are there. Every app is a public client: the document may not ask for another authentication method, and PKCE does the job a client secret would.

Fetching the document means the server requests a URL anyone can type. So the fetch is fenced against reaching the server's own network:

- the URL is https, with a path, and has no query or fragment;
- every address the host resolves to must be public, and the request goes to the address that was checked, so a resolver can't swap in a private one afterwards;
- there are no redirects and no proxy, and the fetch has 5 seconds and 64 KiB at most;
- the document's `client_id` must be the URL it came from.

A redirect URI must be https or on this machine's loopback address. On loopback any port matches, because a native app such as Claude Code listens on a free one (RFC 8252). Anywhere else the URI must be one the document lists, character for character.

Until the redirect URI is trusted, an error stays in the browser as a plain `400`. After that, it goes back to the app as `error` and `error_description`, with the `state`.

## What Claude expects

- It starts signing in only on a `401` that carries `resource_metadata`, and the `resource` in that document must be the URL the person entered.
- It uses only the first authorization server listed.
- Its redirect is `https://claude.ai/api/mcp/auth_callback` from claude.ai, and loopback on any port from Claude Code.
- It renews tokens ahead of time and on a `401`. It waits 10 seconds for an answer, and 30 for a refresh.

## What the database holds

| Table (master) | Lives | What it is |
|---|---|---|
| `oauth_request` | 10 minutes for the consent, then 5 for the code | An authorization waiting for the member, then the code waiting for the app. |
| `oauth_grant` | 90 days | The member's consent to one app in the tenants they chose, and whether the session that gave it proved a second factor. |
| `access_token` | 1 hour | A token of a grant. |
| `refresh_token` | 30 days, never past its grant | A token that gets the grant a new pair, once. |

Access and refresh tokens are opaque: random strings the app holds, of which the database keeps only the SHA-256, as the record key. They are not JWTs, because the endpoint reads the membership on every call anyway, and revoking a token means deleting its row.

Deleting a grant deletes its tokens (`REFERENCE ON DELETE CASCADE`), and so does deleting the account. Deleting a tenant takes it out of the grants that list it (`ON DELETE UNSET`). The sweeper removes expired rows every hour.

## The rules of the token endpoint

- **A code works once.** The exchange deletes the request in the same statement that reads it. A second use of the code means it leaked, so the grant it produced is revoked.
- **A refresh token works once.** Each refresh returns a new pair. A second use of the old refresh token means one of its two holders is an attacker, so the whole grant is revoked and both lose access.
- **A refresh checks again.** The `client_id` must be the grant's, the grant must be alive and the account active. Otherwise the grant is deleted and the app has to ask the member again. Memberships are not checked here, because every call reads them.
- **Revocation** answers `200` whether the token existed or not. An access token is revoked alone; a refresh token takes its grant with it.
- **Second factor.** A tenant that requires one refuses a grant whose consent didn't prove it, both at the consent and on every call.

Responses of the token endpoint carry `Cache-Control: no-store`, and its errors have the RFC 6749 shape, `{"error", "error_description"}`. No cookie authenticates `/oauth/*`, so those routes have no same-origin check.

## Audit

The master's `audit_event` records these events, with the app's `client_id` in `client`:

- `app_allowed` and `app_refused` at the consent, with the browser's address;
- `access_token_issued` on each exchange and refresh;
- `refresh_token_reused` when a code or a refresh token comes back a second time;
- `grant_revoked` on revocation.

Every event except `app_refused` lists the grant's tenants in `tenants`. What the agent then does in a tenant is in that tenant's audit (How a Call Runs).
