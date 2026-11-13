use std::fmt;
use std::ops::Bound;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use surrealdb::engine::any::Any;
use surrealdb::method::Query;
use surrealdb::types::{RecordId, RecordIdKey, RecordIdKeyRange, SurrealValue, Value};
use uuid::Uuid;

use crate::Error;

/// Rows in a page when the caller asks for no size
const DEFAULT_LIMIT: u32 = 20;
/// The most rows a page holds, whatever the caller asks
const MAX_LIMIT: u32 = 200;

/// The page a caller asks for
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct PageRequest {
    /// The `next_cursor` of the previous page; absent on the first one
    pub after: Option<Cursor>,
    /// Rows wanted: 20 when absent, at least 1 and at most 200
    pub limit: Option<u32>,
}

impl PageRequest {
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
}

/// Where a page starts: the id of the last row of the page before it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor(Uuid);

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Uuid::try_parse(&raw)
            .map(Self)
            .map_err(|_| serde::de::Error::custom(InvalidCursor))
    }
}

struct InvalidCursor;

impl fmt::Display for InvalidCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid cursor")
    }
}

/// One page of rows, and where the next one starts
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// `None` on the last page
    pub next_cursor: Option<Cursor>,
}

impl<T> Page<T> {
    /// Converts every row, keeping the cursor
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            data: self.data.into_iter().map(f).collect(),
            next_cursor: self.next_cursor,
        }
    }
}

/// Runs a list query for one page, binding `$range` and `$limit`; the rows come from the last
/// statement, so a `LET` can come first, and every row must carry its `id`
pub async fn page<T: SurrealValue>(
    query: Query<'_, Any>,
    request: PageRequest,
) -> Result<Page<T>, Error> {
    let limit = request.limit() as usize;
    let mut response = query
        .bind(("range", after(request.after.map(Cursor::key))))
        .bind(("limit", i64::from(request.limit()) + 1))
        .await?
        .check()?;
    let statement = response.num_statements().saturating_sub(1);
    let mut rows: Vec<Value> = response.take(statement)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let last = rows.last().map(cursor_of).transpose()?;
    let data = rows
        .into_iter()
        .map(T::from_value)
        .collect::<Result<_, _>>()?;
    Ok(Page {
        data,
        next_cursor: last.filter(|_| more),
    })
}

/// Runs a batch query, binding `$range` and `$limit`, until a batch comes back short; the last
/// statement must return the batch's ids
///
/// ```surql
/// LET $batch = (SELECT VALUE id FROM type::record('person', $range) WHERE … LIMIT $limit);
/// UPDATE $batch SET … RETURN NONE;
/// RETURN $batch;
/// ```
pub async fn write_in_batches<'a>(
    limit: u32,
    mut query: impl FnMut() -> Query<'a, Any>,
) -> Result<u64, Error> {
    let mut last = None;
    let mut written = 0;
    loop {
        let mut response = query()
            .bind(("range", after(last.take())))
            .bind(("limit", i64::from(limit)))
            .await?
            .check()?;
        let statement = response.num_statements().saturating_sub(1);
        let batch: Vec<RecordId> = response.take(statement)?;
        written += batch.len() as u64;
        match batch.last() {
            Some(RecordId { key, .. }) if batch.len() >= limit as usize => last = Some(key.clone()),
            _ => return Ok(written),
        }
    }
}

impl Cursor {
    fn key(self) -> RecordIdKey {
        RecordIdKey::Uuid(self.0.into())
    }
}

fn after(key: Option<RecordIdKey>) -> RecordIdKeyRange {
    RecordIdKeyRange {
        start: key.map_or(Bound::Unbounded, Bound::Excluded),
        end: Bound::Unbounded,
    }
}

fn cursor_of(row: &Value) -> Result<Cursor, Error> {
    match row {
        Value::Object(fields) => match fields.get("id") {
            Some(Value::RecordId(RecordId {
                key: RecordIdKey::Uuid(key),
                ..
            })) => Ok(Cursor(key.into_inner())),
            _ => Err(Error::RowWithoutId),
        },
        _ => Err(Error::RowWithoutId),
    }
}

#[cfg(test)]
mod tests {
    use resysto_id::{Entity, Id};
    use surrealdb::Surreal;

    use super::*;
    use crate::tests::{connect, new_tenant};

    enum Row {}

    impl Entity for Row {
        const TABLE: &'static str = "row";
    }

    #[derive(Debug, PartialEq, SurrealValue)]
    struct RowView {
        id: Id<Row>,
        n: i64,
    }

    #[derive(SurrealValue)]
    struct Hits {
        n: i64,
        hits: i64,
    }

    async fn session_with_rows(count: i64) -> Surreal<Any> {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let session = db.system_session(&tenant).await.unwrap();
        session
            .query(
                "DEFINE TABLE row SCHEMAFULL;
                 DEFINE FIELD n ON row TYPE int;
                 DEFINE FIELD hits ON row TYPE int DEFAULT 0;
                 FOR $n IN array::range(0, $count) {
                     CREATE type::record('row', rand::uuid::v7()) SET n = $n;
                 };",
            )
            .bind(("count", count))
            .await
            .unwrap()
            .check()
            .unwrap();
        session
    }

    #[test]
    fn the_limit_defaults_to_20_and_stays_between_1_and_200() {
        for (asked, got) in [(None, 20), (Some(0), 1), (Some(50), 50), (Some(1000), 200)] {
            let request = PageRequest {
                after: None,
                limit: asked,
            };
            assert_eq!(request.limit(), got, "asked {asked:?}");
        }
    }

    #[tokio::test]
    async fn pages_return_every_matching_row_once_in_id_order() {
        let session = session_with_rows(100).await;
        // 40 rows pass, so the second page holds exactly `limit` rows and must be the last
        // a LET before the list, as relation filters have: the rows come from the last statement
        let list = "LET $even = (SELECT VALUE id FROM row WHERE n % 2 = 0);
                    SELECT id, n FROM type::record('row', $range)
                    WHERE ($min = NONE OR n >= $min) AND id IN $even
                    LIMIT $limit";

        let mut seen = Vec::new();
        let mut request = PageRequest::default();
        let mut pages = 0;
        loop {
            let page: Page<RowView> = page(session.query(list).bind(("min", 20)), request)
                .await
                .unwrap();
            pages += 1;
            seen.extend(page.data);
            match page.next_cursor {
                Some(cursor) => request.after = Some(cursor),
                None => break,
            }
        }

        let expected: Vec<RowView> = session
            .query("SELECT id, n FROM row WHERE n >= 20 AND n % 2 = 0 ORDER BY id")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(expected.len(), 40);
        assert_eq!(seen, expected);
        assert_eq!(pages, 2);
    }

    #[tokio::test]
    async fn a_row_without_its_id_is_an_error() {
        let session = session_with_rows(1).await;
        let list = "SELECT n FROM type::record('row', $range) LIMIT $limit";

        let result = page::<Value>(session.query(list), PageRequest::default()).await;

        assert!(matches!(result, Err(Error::RowWithoutId)), "{result:?}");
    }

    #[tokio::test]
    async fn batches_write_every_matching_record_once() {
        let session = session_with_rows(45).await;
        // the filter still matches what the batch wrote: only the range keeps it from coming back
        let sql = "LET $batch = (SELECT VALUE id FROM type::record('row', $range)
                                 WHERE n % 2 = 0 LIMIT $limit);
                   UPDATE $batch SET hits += 1 RETURN NONE;
                   RETURN $batch;";

        let written = write_in_batches(10, || session.query(sql)).await.unwrap();

        assert_eq!(written, 23);
        let rows: Vec<Hits> = session
            .query("SELECT n, hits FROM row")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(rows.len(), 45);
        for row in rows {
            assert_eq!(row.hits, i64::from(row.n % 2 == 0), "row {}", row.n);
        }
    }
}
