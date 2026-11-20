# Audit

A GRC tool has to answer, for any record, who changed it, through which channel, when, and what it held before. Resysto records that in the database itself, so no code path can skip it: a handler, a future import job, or a bug that writes a record leaves the same trace.

Two tables in every tenant database do the recording, and a third in the master:

| Table | What it holds | Who writes it |
|---|---|---|
| `audit_change` (tenant) | one row per record created, updated or deleted, with the record before and after | an event on each table, in the transaction of the write |
| `audit_event` (tenant) | what happened without changing a record: a request or a tool call the policies refused | the server |
| `audit_event` (master) | logins, failed logins, logouts, second factors, password changes, ended sessions, new accounts, new members, unlocked emails; apps allowed or refused, issued access tokens, reused refresh tokens, revoked grants | `resysto-auth` |

Every row's key is a UUID v7, so the keys sort by time: lists read the newest first, and the retention deletes a range of keys.

## Who wrote a record

Every table of a bounded context has two fields next to its timestamps:

```sql
DEFINE FIELD OVERWRITE created_at ON person TYPE datetime DEFAULT time::now() READONLY;
-- NONE when the system wrote, such as a migration
DEFINE FIELD OVERWRITE created_by ON person TYPE option<record<member>> DEFAULT $auth READONLY;
DEFINE FIELD OVERWRITE updated_at ON person TYPE datetime VALUE time::now();
DEFINE FIELD OVERWRITE updated_by ON person TYPE option<record<member>> VALUE $auth;
```

`$auth` is the member the request's record session runs as. The database fills both fields: a client can't send them, because the input structs don't know them, `READONLY` keeps `created_by` from changing after the create, and the `VALUE` of `updated_by` replaces whatever a query sets. `Record<E, T>` carries them to the API as `created_by` and `updated_by`, the member's id or `null`.

A deleted record can't say who deleted it. Its last row in `audit_change` does.

## The audit event

Every table of a bounded context ends with the same event, which writes one row per changed record:

```sql
-- ------------------------------
-- EVENTS
-- ------------------------------

DEFINE EVENT OVERWRITE audit ON person THEN {
    CREATE type::record('audit_change', rand::uuid::v7()) CONTENT {
        after: $after,
        before: $before,
        by: $auth,
        client: $token.client,
        event: $event,
        ip: $token.ip,
        record: $value.id,
        request: $token.rid,
        session: $token.sid,
        user_agent: $token.ua,
        via: $token.via,
    };
};
```

The event is synchronous, so the row commits or rolls back with the write it records. Events run without permission checks, which is why they can write `audit_change`, a table whose `PERMISSIONS NONE` keeps every record session out, readers and writers alike.

A new table copies the block and changes the table name. The table's tests check it: a write through a record session leaves its row with the right `by`.

## Where the actor comes from

The server opens a record session for each request with a token it signs itself. Besides the grant the permissions read, the token says who is writing and how:

| Claim | Read as | Holds |
|---|---|---|
| `ID` | `$auth` | `member:<uuid>`, the member, whose key is its account's |
| `via` | `$token.via` | the channel: `ui` for the SPA, `mcp` for an agent; `job` arrives with the jobs |
| `client` | `$token.client` | for `mcp`, the `client_id` of the agent's app |
| `sid` | `$token.sid` | the key of the browser session, or of the agent's access token |
| `rid` | `$token.rid` | the request's `x-request-id`, shared by every change the request makes |
| `ip` | `$token.ip` | the address the request came from |
| `ua` | `$token.ua` | the browser's `User-Agent`, cut at 200 characters |

The server makes every `x-request-id` itself, a UUID v7, replacing any the client sent, and answers with it, so a user can quote it when something goes wrong. The `User-Agent` is what the browser says it is: useful to tell devices apart, not proof of anything.

Field clauses and events read the token. They don't see session parameters set with `db.set(…)`, and `LET $token` or `LET $auth` fails, because both are protected. Whatever the token says, only the server could have signed it.

A write through a system session has no member: `by` and `updated_by` stay `NONE`. That is right for a migration and wrong for anything a person or an agent does, so a bounded context writes only through the request's `TenantDb`.

## The client's address

Where the address comes from depends on what stands in front of the server, so it is configuration: `RESYSTO_CLIENT_IP_SOURCE`, one of the sources of `axum-client-ip`.

| Deployment | Value |
|---|---|
| clients reach the server directly, as in development | `ConnectInfo`, the peer of the TCP connection |
| a reverse proxy that appends the peer to `X-Forwarded-For`, such as Traefik or nginx with `$proxy_add_x_forwarded_for` | `RightmostXForwardedFor` |
| a proxy that sets `X-Real-IP` | `XRealIp` |

With one proxy in front, the rightmost address of `X-Forwarded-For` is the one it added, and the client can't forge it; the leftmost is whatever the client wrote. A second proxy, such as a CDN, would make the rightmost address its own. Choosing wrong fails in one of two ways: `ConnectInfo` behind a proxy records the proxy's address on every row, and a header source with no proxy in front lets a client claim any address. A request that lacks the header records no address.

The master's `audit_event` rows carry the same address and browser, for logins, failed logins and every other event of a request.

## The member

`member` is the tenant's own record of who belongs to it. Its key is the uuid of the member's account, and it holds the account's email. `resysto_auth::add_member` writes it when it makes the account a member of the tenant, before the membership in the master: if the second write fails, the tenant is left with a name nobody uses, never with a membership whose author it can't name. A change of email will update the members of the account the same way. Nothing else lives there: roles and organizations stay in the master and in the token. A member is never deleted, so `created_by` keeps pointing at it after the account leaves the tenant.

## Reading the audit

```
GET /t/{slug}/api/v1/audit/changes                          every change, newest first
GET /t/{slug}/api/v1/audit/changes?record=person:<uuid>     the history of one record
GET /t/{slug}/api/v1/audit/events                           the refused requests
```

The lists are paged like the others, with `after` and `limit`. Each change carries `table`, `record`, `event`, `by`, `by_email`, `via`, `client`, `session`, `request`, `ip`, `user_agent`, `before` and `after`. Only a `tenant/admin` who reaches the whole tenant may read them, since the audit shows every record of the tenant: the policy `audit/whole-tenant-admins` refuses everyone else. The handlers then read with a system session, because no record session sees the audit tables.

## Retention

A row stays one year. The hourly sweep of the server deletes older rows from the master and from every ready or suspended tenant, in batches, by key range:

```sql
SELECT VALUE id FROM type::record($table, ..rand::uuid::v7(time::now() - $kept)) LIMIT $limit
```

The range reads only the expired rows. A `WHERE at < …` would scan the whole table every hour.

## What it costs

Measured on SurrealDB 4.0.0, batches of 1,000 rows written through a record session: an insert takes about 10 µs a row with no audit, 22 µs with the two `_by` fields, and 33 µs with an event that writes its row; an update takes 20, 37 and 47 µs. On a request that changes one record the difference doesn't show. On an import of 170,000 rows it is a few seconds and 170,000 audit rows. A table filled mostly by imports can limit its event to people and agents with `WHEN $token.via != 'job'`, and let the job record one event for each run.
