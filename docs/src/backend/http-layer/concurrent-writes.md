# Concurrent Writes

Changing a record over HTTP takes two requests: a `GET` that loads it into a form, and a `PATCH` that saves the form, sometimes minutes later. Nothing stops someone else from saving in between. When that happens, the second save silently overwrites the first, and neither user finds out:

```
10:00  Alice opens Bob's profile    roles = [viewer]
10:01  Carol opens Bob's profile    roles = [viewer]
10:02  Carol adds editor            PATCH { "roles": ["viewer", "editor"] }   -> 200
10:05  Alice adds auditor           PATCH { "roles": ["viewer", "auditor"] }  -> 200

result: [viewer, auditor]. Carol's change is gone.
```

This is the lost update problem. A partial `PATCH` doesn't prevent it: an array is replaced whole, and on a plain field the last writer wins anyway. Agents run into it just like people do. An agent reads a record, reasons about it for half a minute, and writes based on what it read. A database transaction can't help either, because it lives for a few milliseconds inside one request, while Alice's read happened in another request minutes earlier. And nobody wants a transaction held open while a person stares at a form.

The standard fix is optimistic locking. Every write says which version of the record it was based on, and the server refuses the write if the record has changed since. HTTP has had the headers for it for a long time.

## Versions and ETags

Every entity that clients can change carries a `version` number, which the database raises on every write, whoever makes it. A read returns it in the body and in the `ETag` header, and an update or a delete sends it back in `If-Match`:

```
GET    /api/v1/users/0193a2f4-...   -> 200, ETag: "3", { "data": { ..., "version": 3 } }
PATCH  /api/v1/users/0193a2f4-...   If-Match: "3"   { "bio": "Security lead" }
DELETE /api/v1/users/0193a2f4-...   If-Match: "3"
```

| Request | Response |
|---|---|
| a `PATCH` or `DELETE` without `If-Match` | `428 Precondition Required` |
| an `If-Match` that isn't a quoted version (`3`, `*`, `W/"3"`) | `400 Bad Request` |
| a version the record no longer has | `412 Precondition Failed`, and nothing is written |
| the current version | `200` with the record and its new `ETag`, or `204` for a delete |

The header is required. We could honor `If-Match` only when a client sends it, but an optional check gets skipped by exactly the clients that need it most, like a quick script or an agent that never read the docs.

Only entities that clients can change need a version. Tables that only the server writes, such as sessions or login attempts, have no stale reads to protect against. An entity gets its version the day a client can change it.

## What the client does

The client treats the version as opaque. It copies the value from the `ETag` of its last read, or from the record's `version` field, and sends it back unchanged. It never computes one.

On a `412`, the client reads the record again, applies its change to what it finds, and decides whether to try again. The UI tells the user that someone else changed the record and shows the fresh data; an agent re-reads and decides again. The error message includes the current version, but copying it into a retry defeats the purpose: the `412` means the client was about to overwrite data it hasn't seen.

The version also makes retries safe. When a `PATCH` times out, the client can't tell whether it went through. Sending it again with the same `If-Match` either applies it or gets a `412`, so the change never lands twice.

## Keeping the version in the database

The database owns the version, with a field that raises itself on every write:

```sql
DEFINE FIELD OVERWRITE version ON user TYPE int VALUE ($before ?? 0) + 1;
```

A create sets it to 1, and every `UPDATE` or `MERGE` raises it by one. That includes a backfill written in plain SurrealQL, an empty `MERGE {}`, and a write that tries to set `version` itself. No write path can forget it, and no client can choose it.

One detail is easy to get wrong: inside `DEFINE FIELD`, `$before` is the previous value of the field itself, not of the whole record. `$before.version` would stay at 1 forever.

Adding `version` to a table that already has rows means adding a required field to a populated table. It needs a manifest with a `migrate` step that gives the existing rows version 1 (see [Adding a required field](../database/changing-fields.md#adding-a-required-field)).

## Keeping the version out of the entity

The entity struct holds business data only. The id, the version, and the timestamps are written by the server alone, so they don't belong in `User`, where any code could set them. They live in a generic `Record`, which wraps the entity's data and stays flat in the database and in JSON:

```rust
#[derive(Debug, Serialize, SurrealValue)]
pub struct Record<E: Entity, T: SurrealValue> {
    pub id: Id<E>,
    pub version: i64,
    pub created_at: Datetime,
    pub updated_at: Datetime,
    #[serde(flatten)]
    #[surreal(flatten)]
    pub data: T,
}

pub type UserRecord = Record<entity::User, User>;
// { "id": "...", "version": 3, "created_at": "...", "updated_at": "...", "name": "Bob", ... }
```

Reads and writes return the record, while inputs never contain these fields. Since input structs reject unknown fields, a client that sends `version` in a body gets a `400` instead of the illusion that it set something.

## The extractor and the response

Two small pieces carry the version over HTTP. An `IfMatch` extractor reads the header and answers `428` or `400` before the handler runs:

```rust
pub struct IfMatch(pub i64);

impl<S: Send + Sync> FromRequestParts<S> for IfMatch {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let value = parts
            .headers
            .get(header::IF_MATCH)
            .ok_or(AppError::PreconditionRequired)?;
        value
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix('"')?.strip_suffix('"')?.parse().ok())
            .map(Self)
            .ok_or_else(|| {
                AppError::Validation(r#"If-Match must be the ETag of the record, such as "3""#.into())
            })
    }
}
```

`Tagged` is a response that writes the version into the `ETag` header, next to the usual `ApiResponse` body:

```rust
pub struct Tagged<T> {
    pub version: i64,
    pub data: T,
}

impl<T: Serialize> IntoResponse for Tagged<T> {
    fn into_response(self) -> Response {
        let mut response = Json(ApiResponse { data: self.data }).into_response();
        if let Ok(etag) = HeaderValue::try_from(format!("\"{}\"", self.version)) {
            response.headers_mut().insert(header::ETAG, etag);
        }
        response
    }
}
```

A handler that changes a record takes the version from `IfMatch` and returns the new one with `Tagged`:

```rust
async fn update_user(
    State(state): State<AppState>,
    Path(id): Path<UserId>,
    IfMatch(version): IfMatch,
    ValidatedJson(patch): ValidatedJson<UserPatch>,
) -> AppResult<Tagged<UserRecord>> {
    let user = state.user_service.update(id, version, patch).await?;
    Ok(Tagged { version: user.version, data: user })
}
```

## Comparing versions

The comparison happens in the operation, inside the same transaction as the write: read the record, compare the versions, write.

```rust
let Some(user) = store::find(tx, id).await? else {
    return Err(UserError::NotFound);
};
if user.version != version {
    return Err(UserError::Stale(user.version));
}
// apply the patch and write
```

`Stale` maps to `412`. It's tempting to push the check into the query with `UPDATE ... WHERE version = $version`, but when that matches no row it returns `NONE`, and we can't tell a stale version from a missing record or a missing permission.

Two `PATCH`es that arrive at the same moment with the same version both pass the comparison. They can't both commit, though: one of the two transactions hits a write conflict at commit, runs again, reads the new version, and gets its `412`.

## Further reading

- [RFC 9110, `If-Match`](https://www.rfc-editor.org/rfc/rfc9110.html#name-if-match): the header exists to prevent the lost update problem.
- [RFC 6585, `428`](https://www.rfc-editor.org/rfc/rfc6585.html#section-3): for servers that require conditional requests.
- [Martin Fowler, Optimistic Offline Lock](https://martinfowler.com/eaaCatalog/optimisticOfflineLock.html): the pattern, with a version number on each record.
- [Google AIP-154](https://google.aip.dev/154) and the [Kubernetes API conventions](https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#concurrency-control-and-consistency): the same idea as `etag` and `resourceVersion`.
