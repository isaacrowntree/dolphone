//! Messages, the call log, blocked numbers and settings, in one SQLite file.
//!
//! The file lives under `~/.local/share/dolphone/` with 0600 permissions: `/home` is
//! the subvolume every Flipper OS profile shares and the one a factory reset keeps,
//! so this is personal data that outlives a "wipe" unless [`Store::wipe`] is used.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{CallRecord, Direction, Message, MessageState, Outcome, Thread};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Store {
    db: Connection,
    path: Option<PathBuf>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id        INTEGER PRIMARY KEY,
    peer      TEXT NOT NULL,
    direction INTEGER NOT NULL,
    body      TEXT NOT NULL,
    at        INTEGER NOT NULL,
    state     INTEGER NOT NULL,
    read      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS messages_peer ON messages(peer, at);
CREATE TABLE IF NOT EXISTS calls (
    id        INTEGER PRIMARY KEY,
    peer      TEXT NOT NULL,
    direction INTEGER NOT NULL,
    started   INTEGER NOT NULL,
    duration  INTEGER NOT NULL,
    outcome   INTEGER NOT NULL,
    seen      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS calls_started ON calls(started);
CREATE TABLE IF NOT EXISTS blocked (peer TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
PRAGMA user_version = 1;

";

/// Where the store lives for the current user.
pub fn default_path() -> PathBuf {
    data_dir().join("dolphone.db")
}

/// `$XDG_DATA_HOME/dolphone`, or `~/.local/share/dolphone`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("dolphone")
}

/// Contacts sit beside the store, not at the top of `~`, for the same reason.
pub fn contacts_dir() -> PathBuf {
    data_dir().join("contacts")
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let db = Connection::open(path)?;
        restrict(path)?;
        // Write-ahead logging survives power loss mid-write without corrupting the
        // file, and FULL syncs each commit: a text is not "stored" until it is.
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        db.execute_batch(SCHEMA)?;
        // An unreadable file opens fine and fails here; make that an error now, not
        // on the first text.
        db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?;
        Ok(Self { db, path: Some(path.to_path_buf()) })
    }

    pub fn in_memory() -> Result<Self> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db, path: None })
    }

    // Messages

    pub fn add_message(
        &self,
        peer: &str,
        direction: Direction,
        body: &str,
        at: i64,
        state: MessageState,
    ) -> Result<i64> {
        // Your own messages are read by definition.
        let read = direction == Direction::Outgoing;
        self.db.execute(
            "INSERT INTO messages (peer, direction, body, at, state, read) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![peer, direction as u8, body, at, state as u8, read],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Whether this exact message is already stored: a modem that keeps its copies
    /// hands them over again every time phoned starts.
    pub fn has_message(&self, peer: &str, direction: Direction, at: i64, body: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE peer = ?1 AND direction = ?2 AND at = ?3 AND body = ?4)",
            params![peer, direction as u8, at, body],
            |r| r.get(0),
        )?)
    }

    pub fn set_message_state(&self, id: i64, state: MessageState) -> Result<()> {
        self.db.execute("UPDATE messages SET state = ?1 WHERE id = ?2", params![state as u8, id])?;
        Ok(())
    }

    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, peer, direction, body, at, state, read FROM messages WHERE id = ?1",
                params![id],
                row_message,
            )
            .optional()?)
    }

    /// A conversation, oldest first.
    pub fn conversation(&self, peer: &str) -> Result<Vec<Message>> {
        let mut q = self.db.prepare(
            "SELECT id, peer, direction, body, at, state, read FROM messages WHERE peer = ?1 ORDER BY at, id",
        )?;
        let rows = q.query_map(params![peer], row_message)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn mark_read(&self, peer: &str) -> Result<usize> {
        Ok(self.db.execute("UPDATE messages SET read = 1 WHERE peer = ?1 AND read = 0", params![peer])?)
    }

    /// One row per peer, most recent conversation first.
    pub fn threads(&self) -> Result<Vec<Thread>> {
        let mut q = self.db.prepare(
            "SELECT m.peer, m.body, m.at, m.direction,
                    (SELECT COUNT(*) FROM messages u WHERE u.peer = m.peer AND u.read = 0)
             FROM messages m
             WHERE m.id = (SELECT id FROM messages x WHERE x.peer = m.peer ORDER BY at DESC, id DESC LIMIT 1)
             ORDER BY m.at DESC",
        )?;
        let rows = q.query_map([], |r| {
            Ok(Thread {
                peer: r.get(0)?,
                last_body: r.get(1)?,
                last_at: r.get(2)?,
                last_direction: dir(r.get(3)?),
                unread: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn unread_messages(&self) -> Result<u32> {
        Ok(self.db.query_row("SELECT COUNT(*) FROM messages WHERE read = 0", [], |r| r.get(0))?)
    }

    pub fn delete_thread(&self, peer: &str) -> Result<usize> {
        Ok(self.db.execute("DELETE FROM messages WHERE peer = ?1", params![peer])?)
    }

    // Calls

    pub fn add_call(
        &self,
        peer: &str,
        direction: Direction,
        started: i64,
        duration: i64,
        outcome: Outcome,
    ) -> Result<i64> {
        // Only a missed call is news; everything else the user was there for.
        let seen = outcome != Outcome::Missed;
        self.db.execute(
            "INSERT INTO calls (peer, direction, started, duration, outcome, seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![peer, direction as u8, started, duration, outcome as u8, seen],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// The call log, newest first.
    pub fn recents(&self, limit: u32) -> Result<Vec<CallRecord>> {
        let mut q = self.db.prepare(
            "SELECT id, peer, direction, started, duration, outcome, seen FROM calls ORDER BY started DESC, id DESC LIMIT ?1",
        )?;
        let rows = q.query_map(params![limit], |r| {
            Ok(CallRecord {
                id: r.get(0)?,
                peer: r.get(1)?,
                direction: dir(r.get(2)?),
                started: r.get(3)?,
                duration: r.get(4)?,
                outcome: Outcome::from_u8(r.get(5)?),
                seen: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn missed_calls(&self) -> Result<u32> {
        Ok(self.db.query_row("SELECT COUNT(*) FROM calls WHERE seen = 0", [], |r| r.get(0))?)
    }

    pub fn mark_calls_seen(&self) -> Result<usize> {
        Ok(self.db.execute("UPDATE calls SET seen = 1 WHERE seen = 0", [])?)
    }

    // Blocked numbers

    pub fn block(&self, peer: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO blocked (peer) VALUES (?1)", params![peer])?;
        Ok(())
    }

    pub fn unblock(&self, peer: &str) -> Result<()> {
        self.db.execute("DELETE FROM blocked WHERE peer = ?1", params![peer])?;
        Ok(())
    }

    pub fn blocked(&self) -> Result<Vec<String>> {
        let mut q = self.db.prepare("SELECT peer FROM blocked ORDER BY peer")?;
        let rows = q.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn is_blocked(&self, peer: &str) -> Result<bool> {
        Ok(self.blocked()?.iter().any(|b| crate::number::same_party(b, peer)))
    }

    // Settings

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Delete every message, call, block and setting. The file stays, empty, so the
    /// daemon does not have to reopen it.
    pub fn wipe(&self) -> Result<()> {
        self.db.execute_batch(
            "DELETE FROM messages; DELETE FROM calls; DELETE FROM blocked; DELETE FROM settings; VACUUM;",
        )?;
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

fn dir(v: u8) -> Direction {
    if v == 0 {
        Direction::Incoming
    } else {
        Direction::Outgoing
    }
}

fn row_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        peer: r.get(1)?,
        direction: dir(r.get(2)?),
        body: r.get(3)?,
        at: r.get(4)?,
        state: MessageState::from_u8(r.get(5)?),
        read: r.get(6)?,
    })
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_and_unread() {
        let s = Store::in_memory().unwrap();
        s.add_message("+61491570156", Direction::Incoming, "hi", 10, MessageState::Received).unwrap();
        s.add_message("+61491570156", Direction::Outgoing, "hey", 20, MessageState::Sent).unwrap();
        s.add_message("Telstra", Direction::Incoming, "Your bill", 15, MessageState::Received).unwrap();
        let t = s.threads().unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].peer, "+61491570156");
        assert_eq!(t[0].last_body, "hey");
        assert_eq!(t[0].unread, 1);
        assert_eq!(s.unread_messages().unwrap(), 2);
        s.mark_read("+61491570156").unwrap();
        assert_eq!(s.unread_messages().unwrap(), 1);
        assert_eq!(s.conversation("+61491570156").unwrap().len(), 2);
    }

    #[test]
    fn calls_and_missed() {
        let s = Store::in_memory().unwrap();
        s.add_call("+61491570156", Direction::Incoming, 100, 0, Outcome::Missed).unwrap();
        s.add_call("+61491570157", Direction::Outgoing, 200, 65, Outcome::Answered).unwrap();
        assert_eq!(s.missed_calls().unwrap(), 1);
        let r = s.recents(10).unwrap();
        assert_eq!(r[0].peer, "+61491570157");
        assert_eq!(r[1].outcome, Outcome::Missed);
        s.mark_calls_seen().unwrap();
        assert_eq!(s.missed_calls().unwrap(), 0);
    }

    #[test]
    fn blocking_matches_across_forms() {
        let s = Store::in_memory().unwrap();
        s.block("+61491570156").unwrap();
        assert!(s.is_blocked("0491570156").unwrap());
        s.unblock("+61491570156").unwrap();
        assert!(!s.is_blocked("0491570156").unwrap());
    }

    #[test]
    fn file_is_private_and_wipe_empties_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dolphone.db");
        let s = Store::open(&path).unwrap();
        s.add_message("x", Direction::Incoming, "y", 1, MessageState::Received).unwrap();
        s.set_setting("ringer", "pulse").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        s.wipe().unwrap();
        assert!(s.threads().unwrap().is_empty());
        assert_eq!(s.setting("ringer").unwrap(), None);
    }
}
