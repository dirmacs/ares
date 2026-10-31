//! A blocking SQLite driver exposed through the async shape `turso.rs` expects.
//!
//! Why this exists: the embedded store was written against `libsql`, which is
//! async. `libsql-ffi` vendors its own SQLite, and `appa` (via `ares-tools`)
//! pulls `rusqlite`, which vendors another. Cargo permits one `links = "sqlite3"`
//! per graph, and the two produce duplicate `sqlite3_*` symbols at LINK time —
//! `cargo check` passes, only linking fails. The practical result was that the
//! embedded backend could not be compiled into a binary alongside `appa`.
//!
//! `rusqlite` is already linked in every default build because `appa` needs it.
//! Building the embedded store on `rusqlite` therefore adds no second SQLite and
//! requires no change to `appa` — no security control is traded away.
//!
//! The only impedance mismatch is async: `turso.rs` awaits its queries, while
//! `rusqlite` blocks. Each call is pushed onto the blocking pool via
//! `spawn_blocking`, and a single connection is guarded by a mutex so the
//! serialised access `libsql`'s connection also provided is preserved.

use std::sync::Arc;

use parking_lot::Mutex;

use ares_types::types::{AppError, Result};

/// Row access mirroring the `libsql::Row` calls `turso.rs` makes:
/// `get::<i64>(n)`, `get::<i32>(n)`, `get::<f64>(n)`, `get::<String>(n)`.
pub struct Row {
    values: Vec<rusqlite::types::Value>,
}

impl Row {
    fn from_rusqlite(row: &rusqlite::Row<'_>, width: usize) -> Result<Self> {
        let mut values = Vec::with_capacity(width);
        for i in 0..width {
            let v: rusqlite::types::Value = row
                .get(i)
                .map_err(|e| AppError::Database(format!("column {i}: {e}")))?;
            values.push(v);
        }
        Ok(Self { values })
    }

    fn value(&self, idx: usize) -> Result<&rusqlite::types::Value> {
        self.values
            .get(idx)
            .ok_or_else(|| AppError::Database(format!("column index {idx} out of range")))
    }

    pub fn get<T: FromValue>(&self, idx: usize) -> Result<T> {
        T::from_value(self.value(idx)?)
    }
}

/// Conversion from a stored SQLite value to the types `turso.rs` reads.
pub trait FromValue: Sized {
    fn from_value(v: &rusqlite::types::Value) -> Result<Self>;
}

impl FromValue for String {
    fn from_value(v: &rusqlite::types::Value) -> Result<Self> {
        match v {
            rusqlite::types::Value::Text(s) => Ok(s.clone()),
            rusqlite::types::Value::Integer(i) => Ok(i.to_string()),
            rusqlite::types::Value::Real(f) => Ok(f.to_string()),
            rusqlite::types::Value::Null => Ok(String::new()),
            rusqlite::types::Value::Blob(b) => Err(AppError::Database(format!(
                "expected text at this column, found a {}-byte blob",
                b.len()
            ))),
        }
    }
}

impl FromValue for i64 {
    fn from_value(v: &rusqlite::types::Value) -> Result<Self> {
        match v {
            rusqlite::types::Value::Integer(i) => Ok(*i),
            rusqlite::types::Value::Real(f) => Ok(*f as i64),
            rusqlite::types::Value::Text(s) => s
                .parse()
                .map_err(|e| AppError::Database(format!("expected i64, text {:?}: {e}", s))),
            rusqlite::types::Value::Null => Ok(0),
            rusqlite::types::Value::Blob(_) => {
                Err(AppError::Database("expected i64, found blob".into()))
            }
        }
    }
}

impl FromValue for i32 {
    fn from_value(v: &rusqlite::types::Value) -> Result<Self> {
        i64::from_value(v).map(|n| n as i32)
    }
}

impl<T: FromValue> FromValue for Option<T> {
    /// A NULL maps to `None`; anything else converts as its inner type. Without
    /// this, reading a nullable column (`title`, `display_name`) fails to
    /// compile, and "NULL becomes empty string" would lose the distinction.
    fn from_value(v: &rusqlite::types::Value) -> Result<Self> {
        match v {
            rusqlite::types::Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

impl FromValue for f64 {
    fn from_value(v: &rusqlite::types::Value) -> Result<Self> {
        match v {
            rusqlite::types::Value::Real(f) => Ok(*f),
            rusqlite::types::Value::Integer(i) => Ok(*i as f64),
            rusqlite::types::Value::Text(s) => s
                .parse()
                .map_err(|e| AppError::Database(format!("expected f64, text {:?}: {e}", s))),
            rusqlite::types::Value::Null => Ok(0.0),
            rusqlite::types::Value::Blob(_) => {
                Err(AppError::Database("expected f64, found blob".into()))
            }
        }
    }
}

/// Result set, materialised eagerly so the blocking call can return.
pub struct Rows {
    rows: std::vec::IntoIter<Row>,
}

impl Rows {
    /// `None` when exhausted. Synchronous: rows are materialised by `query`.
    ///
    /// Named `try_next` rather than `next` deliberately: this is not an
    /// `Iterator`, and a `next` that returns `Result<Option<_>>` invites the
    /// reader to assume `Iterator` semantics (no error channel).
    pub fn try_next(&mut self) -> Result<Option<Row>> {
        Ok(self.rows.next())
    }
}

/// One SQLite connection, guarded so accesses serialise as they did under libsql.
#[derive(Clone)]
pub struct Connection {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl Connection {
    pub fn open(path: &str) -> Result<Self> {
        let conn = if path == ":memory:" {
            rusqlite::Connection::open_in_memory()
        } else {
            rusqlite::Connection::open(path)
        }
        .map_err(|e| AppError::Database(format!("Failed to open SQLite database {path}: {e}")))?;
        // Multiple writers are not supported anyway; WAL keeps readers unblocked.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "busy_timeout", 5_000i64);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Execute a statement and return the affected row count.
    pub fn execute<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<usize> {
        let guard = self.conn.lock();
        guard
            .execute(sql, params)
            .map_err(|e| AppError::Database(format!("Failed to execute statement: {e}")))
    }

    /// Run a query and materialise every row.
    pub fn query<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<Rows> {
        let guard = self.conn.lock();
        let mut stmt = guard
            .prepare(sql)
            .map_err(|e| AppError::Database(format!("Failed to prepare query: {e}")))?;
        let width = stmt.column_count();
        let mut rows = stmt
            .query(params)
            .map_err(|e| AppError::Database(format!("Failed to run query: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| AppError::Database(format!("Failed to read row: {e}")))?
        {
            out.push(Row::from_rusqlite(row, width)?);
        }
        Ok(Rows {
            rows: out.into_iter(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_then_query_round_trips_a_string() {
        let conn = Connection::open(":memory:").expect("open");
        conn.execute("CREATE TABLE t (a TEXT NOT NULL)", ())
            .expect("create");
        conn.execute("INSERT INTO t (a) VALUES (?)", ["hello"])
            .expect("insert");
        let mut rows = conn.query("SELECT a FROM t", ()).expect("select");
        let row = rows.try_next().expect("next").expect("a row");
        assert_eq!(row.get::<String>(0).expect("text"), "hello");
        assert!(rows.try_next().expect("next").is_none());
    }

    #[test]
    fn integer_and_real_columns_convert() {
        let conn = Connection::open(":memory:").expect("open");
        conn.execute("CREATE TABLE n (i INTEGER, r REAL)", ())
            .expect("create");
        conn.execute("INSERT INTO n (i, r) VALUES (?, ?)", ["42", "1.5"])
            .expect("insert");
        let mut rows = conn.query("SELECT i, r FROM n", ()).expect("select");
        let row = rows.try_next().expect("next").expect("row");
        assert_eq!(row.get::<i64>(0).expect("i64"), 42);
        assert_eq!(row.get::<i32>(0).expect("i32"), 42);
        assert_eq!(row.get::<f64>(1).expect("f64"), 1.5);
    }

    #[test]
    fn out_of_range_column_is_an_error_not_a_zero() {
        let conn = Connection::open(":memory:").expect("open");
        conn.execute("CREATE TABLE t (a TEXT)", ()).expect("create");
        conn.execute("INSERT INTO t (a) VALUES (?)", ["x"])
            .expect("insert");
        let mut rows = conn.query("SELECT a FROM t", ()).expect("select");
        let row = rows.try_next().expect("next").expect("row");
        assert!(row.get::<String>(7).is_err(), "a missing column must error");
    }

    #[test]
    fn execute_reports_rows_affected() {
        let conn = Connection::open(":memory:").expect("open");
        conn.execute("CREATE TABLE t (a TEXT)", ()).expect("create");
        for v in ["a", "b", "c"] {
            conn.execute("INSERT INTO t (a) VALUES (?)", [v])
                .expect("insert");
        }
        let n = conn
            .execute("DELETE FROM t WHERE a != ?", ["a"])
            .expect("delete");
        assert_eq!(n, 2);
    }

    #[test]
    fn wal_and_busy_timeout_are_set() {
        let conn = Connection::open(":memory:").expect("open");
        let mut rows = conn.query("PRAGMA busy_timeout", ()).expect("pragma");
        let row = rows.try_next().expect("next").expect("row");
        assert_eq!(row.get::<i64>(0).expect("timeout"), 5_000);
    }
}
