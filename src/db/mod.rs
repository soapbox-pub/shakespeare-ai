//! SQLite: connections and migrations. Adapted from npanel's `db/mod.rs`.
//!
//! One database file, opened by a bounded pool of connections. WAL keeps
//! readers from waiting on a writer, and a busy timeout makes two writers take
//! turns rather than fail. Every write that moves money is an `IMMEDIATE`
//! transaction, so the checks inside it see the latest balance.

mod migrations;

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::{Condvar, Mutex};
use rusqlite::{params, Connection, OpenFlags};
use tokio::sync::Semaphore;

const MAX_CONNECTIONS: usize = 8;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    pool: Mutex<Pool>,
    returned: Condvar,
    /// Async callers queue here, before the blocking pool, so a burst waits
    /// as futures rather than as parked blocking threads.
    permits: Arc<Semaphore>,
}

struct Pool {
    idle: Vec<Connection>,
    open: usize,
}

impl Db {
    /// Open the database at `path`, creating and migrating it if necessary.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let db = Db {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                pool: Mutex::new(Pool { idle: Vec::new(), open: 0 }),
                returned: Condvar::new(),
                permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            }),
        };
        db.with(|conn| apply_migrations(conn, migrations::MIGRATIONS))?;
        Ok(db)
    }

    /// Run `f` with a connection, on the calling thread.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        f(self.checkout()?.deref_mut())
    }

    /// Run `f` with a connection on the blocking pool, so a query never holds
    /// up a runtime thread.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let permit = self.inner.permits.clone().acquire_owned().await.expect("the semaphore is never closed");
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            db.with(f)
        })
        .await
        .context("database task panicked")?
    }

    fn checkout(&self) -> Result<Lease<'_>> {
        let mut pool = self.inner.pool.lock();
        loop {
            if let Some(conn) = pool.idle.pop() {
                return Ok(Lease { inner: &self.inner, conn: Some(conn) });
            }
            if pool.open < MAX_CONNECTIONS {
                break;
            }
            self.inner.returned.wait(&mut pool);
        }
        pool.open += 1;
        drop(pool);
        match open_connection(&self.inner.path) {
            Ok(conn) => Ok(Lease { inner: &self.inner, conn: Some(conn) }),
            Err(error) => {
                self.inner.release_slot();
                Err(error)
            }
        }
    }
}

impl Inner {
    fn checkin(&self, conn: Connection) {
        // A connection left mid-transaction — by an error, or by a panic
        // unwinding through it — is not one to reuse.
        if !conn.is_autocommit() {
            drop(conn);
            self.release_slot();
            return;
        }
        self.pool.lock().idle.push(conn);
        self.returned.notify_one();
    }

    fn release_slot(&self) {
        self.pool.lock().open -= 1;
        self.returned.notify_one();
    }
}

/// A checked-out connection, returned to the pool however its user exits.
struct Lease<'a> {
    inner: &'a Inner,
    conn: Option<Connection>,
}

impl Deref for Lease<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("held until dropped")
    }
}

impl DerefMut for Lease<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("held until dropped")
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.inner.checkin(conn);
        }
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    // First, because switching journal modes can block, and without a
    // timeout in force a collision fails outright rather than waiting.
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // FULL, not NORMAL: these rows are money, and a commit that a power cut
    // can undo is a payment credited and then forgotten.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.set_prepared_statement_cache_capacity(64);
    Ok(conn)
}

/// Apply whichever migrations a database is missing, each in a transaction of
/// its own. Append-only: deployed databases have run the old ones.
fn apply_migrations(conn: &mut Connection, migrations: &[(&str, &str)]) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at INTEGER NOT NULL) STRICT")?;
    let applied: std::collections::HashSet<String> =
        conn.prepare("SELECT name FROM schema_migrations")?.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    for (name, sql) in migrations {
        if applied.contains(*name) {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql).with_context(|| format!("migration {name}"))?;
        tx.execute("INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)", params![name, crate::util::now()])?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub mod testing {
    pub struct TempDb {
        pub db: super::Db,
        pub _dir: tempfile::TempDir,
    }

    pub fn temp_db() -> TempDb {
        let dir = tempfile::tempdir().unwrap();
        let db = super::Db::open(&dir.path().join("test.sqlite3")).unwrap();
        TempDb { db, _dir: dir }
    }
}
