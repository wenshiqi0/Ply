//! A whole mount inside one SQLite file: the "mount a database" case.
//!
//! One table, one row per file or directory. Calls run on the caller's thread
//! behind a mutex; each statement is short, so this stays simpler than a
//! blocking pool until a workload shows otherwise.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{KvBackend, KvStorage, Node, StorageInfo};

pub struct SqliteKv {
    conn: Mutex<Connection>,
    location: PathBuf,
}

pub type SqliteStorage = KvStorage<SqliteKv>;

impl SqliteStorage {
    /// Open (creating if needed) the database file at `path`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path).map_err(sql)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS entries (
                 path        TEXT PRIMARY KEY,
                 is_dir      INTEGER NOT NULL,
                 data        BLOB NOT NULL,
                 modified_ms INTEGER NOT NULL
             );",
        )
        .map_err(sql)?;
        KvStorage::new(SqliteKv { conn: Mutex::new(conn), location: path })
    }
}

fn below(key: &str) -> String {
    if key == "/" { "/".to_string() } else { format!("{key}/") }
}

fn sql(err: rusqlite::Error) -> io::Error {
    io::Error::other(format!("sqlite: {err}"))
}

fn row_node(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Node> {
    Ok(Node {
        dir: row.get::<_, i64>(offset)? != 0,
        data: row.get(offset + 1)?,
        modified_ms: row.get(offset + 2)?,
    })
}

impl KvBackend for SqliteKv {
    fn info(&self) -> StorageInfo {
        StorageInfo { kind: "sqlite".into(), location: self.location.display().to_string() }
    }

    fn get(&self, key: &str) -> io::Result<Option<Node>> {
        let conn = self.conn.lock().expect("sqlite kv poisoned");
        conn.query_row(
            "SELECT is_dir, data, modified_ms FROM entries WHERE path = ?1",
            params![key],
            |row| row_node(row, 0),
        )
        .optional()
        .map_err(sql)
    }

    fn put(&self, key: &str, node: Node) -> io::Result<()> {
        let conn = self.conn.lock().expect("sqlite kv poisoned");
        conn.execute(
            "INSERT INTO entries (path, is_dir, data, modified_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 is_dir = excluded.is_dir, data = excluded.data, modified_ms = excluded.modified_ms",
            params![key, node.dir as i64, node.data, node.modified_ms],
        )
        .map(|_| ())
        .map_err(sql)
    }

    fn delete_tree(&self, key: &str) -> io::Result<()> {
        let conn = self.conn.lock().expect("sqlite kv poisoned");
        let prefix = below(key);
        conn.execute(
            "DELETE FROM entries WHERE path = ?1 OR substr(path, 1, length(?2)) = ?2",
            params![key, prefix],
        )
        .map(|_| ())
        .map_err(sql)
    }

    fn descendants(&self, key: &str) -> io::Result<Vec<(String, Node)>> {
        let conn = self.conn.lock().expect("sqlite kv poisoned");
        let prefix = below(key);
        let mut stmt = conn
            .prepare(
                "SELECT path, is_dir, data, modified_ms FROM entries
                 WHERE path != ?1 AND substr(path, 1, length(?2)) = ?2 ORDER BY path",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map(params![key, prefix], |row| Ok((row.get::<_, String>(0)?, row_node(row, 1)?)))
            .map_err(sql)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)
    }
}
