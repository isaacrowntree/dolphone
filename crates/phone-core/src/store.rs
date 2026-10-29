//! Messages, the call log, blocked numbers and settings, in one SQLite file.
//!
//! The file lives under `~/.local/share/dolphone/` with 0600 permissions: `/home` is
//! the subvolume every Flipper OS profile shares and the one a factory reset keeps,
//! so this is personal data that outlives a "wipe" unless [`Store::wipe`] is used.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{CallRecord, Direction, Message, MessageState, Outcome, Part, Thread};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// An emergency alert: id, channel, level, text, when.
pub type AlertRow = (i64, u32, u8, String, i64);
/// A standing grant: id, program, numbers, per hour, until (0 for no end).
pub type GrantRow = (i64, String, String, u32, i64);
/// An audit entry: when, program, action, target, result.
pub type AuditRow = (i64, String, String, String, String);

pub struct Store {
    db: Connection,
    path: Option<PathBuf>,
    /// The SIM in use, noted on each message and call as it is stored.
    sim: std::cell::RefCell<String>,
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
CREATE TABLE IF NOT EXISTS alerts (
    id      INTEGER PRIMARY KEY,
    channel INTEGER NOT NULL,
    level   INTEGER NOT NULL,
    body    TEXT NOT NULL,
    at      INTEGER NOT NULL,
    serial  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS grants (
    id       INTEGER PRIMARY KEY,
    program  TEXT NOT NULL,
    numbers  TEXT NOT NULL,
    per_hour INTEGER NOT NULL,
    until    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS audit (
    id      INTEGER PRIMARY KEY,
    at      INTEGER NOT NULL,
    program TEXT NOT NULL,
    action  TEXT NOT NULL,
    target  TEXT NOT NULL,
    result  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

";

/// Changes since the first schema, applied once each (user_version counts them).
fn migrate(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let has = |table: &str, column: &str| -> Result<bool> {
        let mut q = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = q.query_map([], |r| r.get::<_, String>(1))?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(names.iter().any(|n| n == column))
    };
    if version < 2 {
        // Which SIM a record went through, for multi-SIM and eSIM profile switches.
        for table in ["messages", "calls"] {
            if !has(table, "sim")? {
                db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN sim TEXT NOT NULL DEFAULT ''"))?;
            }
        }
        // Full-text search over message bodies, kept in step by triggers.
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(body, content='messages', content_rowid='id');
             CREATE TRIGGER IF NOT EXISTS messages_fts_add AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(rowid, body) VALUES (new.id, new.body);
             END;
             CREATE TRIGGER IF NOT EXISTS messages_fts_del AFTER DELETE ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, body) VALUES ('delete', old.id, old.body);
             END;
             CREATE TRIGGER IF NOT EXISTS messages_fts_upd AFTER UPDATE OF body ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, body) VALUES ('delete', old.id, old.body);
                 INSERT INTO messages_fts(rowid, body) VALUES (new.id, new.body);
             END;
             INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');
             PRAGMA user_version = 2;",
        )?;
    }
    if version < 3 {
        // Which network the modem was on, and when: for noticing a forced fall back
        // to 2G, and for the owner's own record.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS cells (
                 id INTEGER PRIMARY KEY, at INTEGER NOT NULL, tech TEXT NOT NULL,
                 operator TEXT NOT NULL, registered INTEGER NOT NULL
             );
             PRAGMA user_version = 3;",
        )?;
    }
    if version < 4 {
        // MMS attachments (encode_parts).
        if !has("messages", "parts")? {
            db.execute_batch("ALTER TABLE messages ADD COLUMN parts TEXT NOT NULL DEFAULT ''")?;
        }
        db.execute_batch("PRAGMA user_version = 4;")?;
    }
    Ok(())
}

/// A search as FTS5 understands it: each word a prefix, all of them required, and
/// nothing the user typed read as FTS5's own syntax.
fn fts_query(q: &str) -> String {
    q.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text for an XML attribute.
fn xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

pub use crate::paths::{contacts_dir, data_dir, default_path};

/// Whether a store file is encrypted: a plain SQLite file starts with its format
/// string, and an SQLCipher one with random bytes.
pub fn is_encrypted(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)) {
        Ok(()) => &head != b"SQLite format 3\0",
        Err(_) => false,
    }
}

/// SQLCipher's raw key form: 32 bytes as `x'<hex>'`, used as is, without its
/// passphrase derivation (PRAGMA key in SQLCipher's API documentation).
fn key_pragma(key: &[u8]) -> String {
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    format!("x'{hex}'")
}

/// Copy a store into a new file, encrypted with `to_key` or plain without one,
/// through SQLCipher's sqlcipher_export; then check the copy opens and replace the
/// original with it. The original's WAL is folded in first, and no plain copy is
/// left behind.
pub fn rekey_file(path: &Path, from_key: Option<&[u8]>, to_key: Option<&[u8]>) -> Result<()> {
    let tmp = path.with_extension("db.rekey");
    let _ = std::fs::remove_file(&tmp);
    {
        let db = Connection::open(path)?;
        if let Some(k) = from_key {
            db.pragma_update(None, "key", key_pragma(k))?;
        }
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // An empty key makes the copy plain.
        let key = to_key.map(|k| format!("\"{}\"", key_pragma(k))).unwrap_or_else(|| "''".into());
        db.execute(&format!("ATTACH DATABASE ?1 AS target KEY {key}"), params![tmp.display().to_string()])?;
        db.query_row("SELECT sqlcipher_export('target')", [], |_| Ok(()))?;
        db.execute_batch(&format!("PRAGMA target.user_version = {version}; DETACH DATABASE target;"))?;
    }
    restrict(&tmp)?;
    // The copy must open with its key before the original goes.
    {
        let db = Connection::open(&tmp)?;
        if let Some(k) = to_key {
            db.pragma_update(None, "key", key_pragma(k))?;
        }
        db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?;
    }
    std::fs::rename(&tmp, path)?;
    for side in ["db-wal", "db-shm"] {
        let _ = std::fs::remove_file(path.with_extension(side));
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_keyed(path, None)
    }

    /// Open a store encrypted with this key (SQLCipher), or a plain one without.
    pub fn open_keyed(path: &Path, key: Option<&[u8]>) -> Result<Self> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let db = Connection::open(path)?;
        restrict(path)?;
        if let Some(k) = key {
            // Before anything else reads the file.
            db.pragma_update(None, "key", key_pragma(k))?;
        }
        // Write-ahead logging survives power loss mid-write without corrupting the
        // file, and FULL syncs each commit: a text is not "stored" until it is.
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        db.execute_batch(SCHEMA)?;
        // An unreadable file opens fine and fails here; make that an error now, not
        // on the first text.
        db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?;
        migrate(&db)?;
        Ok(Self { db, path: Some(path.to_path_buf()), sim: Default::default() })
    }

    pub fn in_memory() -> Result<Self> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(SCHEMA)?;
        migrate(&db)?;
        Ok(Self { db, path: None, sim: Default::default() })
    }

    /// Note the SIM now in use (its ICCID); only the last four digits are kept,
    /// enough to tell SIMs apart without storing the whole identifier.
    pub fn set_sim(&self, iccid: &str) {
        let digits: String = iccid.chars().filter(char::is_ascii_digit).collect();
        let short = digits.get(digits.len().saturating_sub(4)..).unwrap_or("").to_string();
        *self.sim.borrow_mut() = short;
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
            "INSERT INTO messages (peer, direction, body, at, state, read, sim) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![peer, direction as u8, body, at, state as u8, read, *self.sim.borrow()],
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

    /// An MMS's attachments, once its files are in place.
    pub fn set_parts(&self, id: i64, parts: &[Part]) -> Result<()> {
        let clean: Vec<Part> = parts
            .iter()
            .filter(|p| !(p.mime.contains(['\t', '\n']) || p.path.contains(['\t', '\n'])))
            .cloned()
            .collect();
        self.db.execute("UPDATE messages SET parts = ?1 WHERE id = ?2", params![encode_parts(&clean), id])?;
        Ok(())
    }

    pub fn set_message_state(&self, id: i64, state: MessageState) -> Result<()> {
        self.db.execute("UPDATE messages SET state = ?1 WHERE id = ?2", params![state as u8, id])?;
        Ok(())
    }

    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, peer, direction, body, at, state, read, sim, parts FROM messages WHERE id = ?1",
                params![id],
                row_message,
            )
            .optional()?)
    }

    /// A conversation, oldest first.
    pub fn conversation(&self, peer: &str) -> Result<Vec<Message>> {
        let mut q = self.db.prepare(
            "SELECT id, peer, direction, body, at, state, read, sim, parts FROM messages WHERE peer = ?1 ORDER BY at, id",
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
                    (SELECT COUNT(*) FROM messages u WHERE u.peer = m.peer AND u.read = 0), m.parts
             FROM messages m
             WHERE m.id = (SELECT id FROM messages x WHERE x.peer = m.peer ORDER BY at DESC, id DESC LIMIT 1)
             ORDER BY m.at DESC",
        )?;
        let rows = q.query_map([], |r| {
            Ok(Thread {
                peer: r.get(0)?,
                last_body: preview(r.get(1)?, &r.get::<_, String>(5)?),
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

    /// Messages whose text has every word searched for (as the start of a word),
    /// newest first.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<Message>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.db.prepare(
            "SELECT m.id, m.peer, m.direction, m.body, m.at, m.state, m.read, m.sim, m.parts
             FROM messages_fts f JOIN messages m ON m.id = f.rowid
             WHERE messages_fts MATCH ?1 ORDER BY m.at DESC, m.id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit], row_message)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Every message as an SMS Backup & Restore file, the format Android's most
    /// used backup app reads, for moving the history to an Android phone. `name`
    /// gives a contact's name for a number, for the optional contact_name field.
    pub fn export_xml(&self, name: impl Fn(&str) -> String) -> Result<String> {
        let mut q = self
            .db
            .prepare("SELECT id, peer, direction, body, at, state, read, sim, parts FROM messages ORDER BY at, id")?;
        let all: Vec<Message> = q.query_map([], row_message)?.collect::<std::result::Result<_, _>>()?;
        let mut out =
            format!("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n<smses count=\"{}\">\n", all.len());
        for m in &all {
            // type: 1 received, 2 sent, 5 failed, 6 queued; status: -1 none, 0 complete.
            let (kind, status) = match (m.direction, m.state) {
                (Direction::Incoming, _) => (1, -1),
                (Direction::Outgoing, MessageState::Failed) => (5, 64),
                (Direction::Outgoing, MessageState::Sending) => (6, 32),
                (Direction::Outgoing, MessageState::Delivered) => (2, 0),
                (Direction::Outgoing, _) => (2, -1),
            };
            let t = crate::when::local(m.at);
            let readable = format!(
                "{} {}, {} {:02}:{:02}",
                ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
                    [t.month as usize % 12],
                t.day,
                t.year,
                t.hour,
                t.minute
            );
            out.push_str(&format!(
                "  <sms protocol=\"0\" address=\"{}\" date=\"{}\" type=\"{kind}\" subject=\"null\" body=\"{}\" toa=\"null\" sc_toa=\"null\" service_center=\"null\" read=\"{}\" status=\"{status}\" locked=\"0\" date_sent=\"0\" readable_date=\"{}\" contact_name=\"{}\" />\n",
                xml_attr(&m.peer),
                m.at * 1000,
                xml_attr(&m.body),
                u8::from(m.read),
                xml_attr(&readable),
                xml_attr(&name(&m.peer)),
            ));
        }
        out.push_str("</smses>\n");
        Ok(out)
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
            "INSERT INTO calls (peer, direction, started, duration, outcome, seen, sim) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![peer, direction as u8, started, duration, outcome as u8, seen, *self.sim.borrow()],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// The call log, newest first.
    pub fn recents(&self, limit: u32) -> Result<Vec<CallRecord>> {
        let mut q = self.db.prepare(
            "SELECT id, peer, direction, started, duration, outcome, seen, sim FROM calls ORDER BY started DESC, id DESC LIMIT ?1",
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
                sim: r.get(7)?,
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

    /// Whether a number is blocked: an exact entry, or a prefix entry ending in `*`
    /// ("+44*" blocks every UK number, "1300*" every 1300 number).
    pub fn is_blocked(&self, peer: &str) -> Result<bool> {
        Ok(self.blocked()?.iter().any(|b| matches_pattern(b, peer)))
    }

    // Emergency alerts

    /// Store an alert, unless it is a repeat: networks broadcast the same alert
    /// again and again, and its channel and serial identify it.
    pub fn add_alert(&self, channel: u32, level: u8, body: &str, at: i64, serial: u32) -> Result<Option<i64>> {
        let seen: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM alerts WHERE channel = ?1 AND serial = ?2 AND body = ?3)",
            params![channel, serial, body],
            |r| r.get(0),
        )?;
        if seen {
            return Ok(None);
        }
        self.db.execute(
            "INSERT INTO alerts (channel, level, body, at, serial) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![channel, level, body, at, serial],
        )?;
        Ok(Some(self.db.last_insert_rowid()))
    }

    /// (id, channel, level, body, at), newest first.
    pub fn alerts(&self, limit: u32) -> Result<Vec<AlertRow>> {
        let mut q =
            self.db.prepare("SELECT id, channel, level, body, at FROM alerts ORDER BY at DESC, id DESC LIMIT ?1")?;
        let rows = q.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // Standing grants

    pub fn add_grant(&self, program: &str, numbers: &str, per_hour: u32, until: i64) -> Result<i64> {
        self.db.execute(
            "INSERT INTO grants (program, numbers, per_hour, until) VALUES (?1, ?2, ?3, ?4)",
            params![program, numbers, per_hour, until],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// (id, program, numbers, per_hour, until)
    pub fn grants(&self) -> Result<Vec<GrantRow>> {
        let mut q = self.db.prepare("SELECT id, program, numbers, per_hour, until FROM grants ORDER BY id")?;
        let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn revoke_grant(&self, id: i64) -> Result<()> {
        self.db.execute("DELETE FROM grants WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The grant that lets `program` text `peer` at `now`, if any: (id, per_hour).
    pub fn grant_for(&self, program: &str, peer: &str, now: i64) -> Result<Option<(i64, u32)>> {
        Ok(self
            .grants()?
            .into_iter()
            .filter(|g| g.1 == program && (g.4 == 0 || g.4 > now))
            .find(|g| g.2.split(',').map(str::trim).any(|p| !p.is_empty() && matches_pattern(p, peer)))
            .map(|g| (g.0, g.3)))
    }

    // Cells

    /// Note a change of network technology or operator. Kept to the last 2000.
    pub fn add_cell(&self, at: i64, tech: &str, operator: &str, registered: bool) -> Result<()> {
        self.db.execute(
            "INSERT INTO cells (at, tech, operator, registered) VALUES (?1, ?2, ?3, ?4)",
            params![at, tech, operator, registered],
        )?;
        self.db.execute("DELETE FROM cells WHERE id <= (SELECT MAX(id) - 2000 FROM cells)", [])?;
        Ok(())
    }

    /// The network history, newest first: (at, technology, operator, registered).
    pub fn cells(&self, limit: u32) -> Result<Vec<(i64, String, String, bool)>> {
        let mut q = self.db.prepare("SELECT at, tech, operator, registered FROM cells ORDER BY id DESC LIMIT ?1")?;
        let rows = q.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // Audit

    pub fn audit(&self, program: &str, action: &str, target: &str, result: &str, at: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO audit (at, program, action, target, result) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![at, program, action, target, result],
        )?;
        // The log is a record, not an archive: keep the last thousand lines.
        self.db.execute("DELETE FROM audit WHERE id <= (SELECT MAX(id) - 1000 FROM audit)", [])?;
        Ok(())
    }

    /// (at, program, action, target, result), newest first.
    pub fn audit_log(&self, limit: u32) -> Result<Vec<AuditRow>> {
        let mut q =
            self.db.prepare("SELECT at, program, action, target, result FROM audit ORDER BY id DESC LIMIT ?1")?;
        let rows = q.query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
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
            "DELETE FROM messages; DELETE FROM calls; DELETE FROM blocked; DELETE FROM settings; \
             DELETE FROM alerts; DELETE FROM grants; DELETE FROM audit; DELETE FROM cells; VACUUM;",
        )?;
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// A blocklist or grant entry against a number: equal parties, or a prefix when the
/// entry ends in `*` (compared on digits, so "+44*" and "0044*" mean the same).
pub fn matches_pattern(pattern: &str, peer: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => {
            let (p, n) = (crate::number::digits(prefix), crate::number::digits(peer));
            !p.is_empty() && (n.starts_with(&p) || peer.starts_with(prefix))
        }
        None => crate::number::same_party(pattern, peer),
    }
}

fn dir(v: u8) -> Direction {
    if v == 0 {
        Direction::Incoming
    } else {
        Direction::Outgoing
    }
}

/// What a thread shows for its last message: the text, or for an MMS with none,
/// what it carried ("[Picture]").
fn preview(body: String, parts: &str) -> String {
    match decode_parts(parts).first() {
        Some(p) if body.is_empty() => format!("[{}]", p.kind()),
        _ => body,
    }
}

/// Parts as stored: one "mime<TAB>path" a line (neither can hold a tab or a
/// newline: MIME types are tokens, and phoned names the files).
fn encode_parts(parts: &[Part]) -> String {
    parts.iter().map(|p| format!("{}\t{}", p.mime, p.path)).collect::<Vec<_>>().join("\n")
}

fn decode_parts(s: &str) -> Vec<Part> {
    s.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(mime, path)| Part { mime: mime.into(), path: path.into() })
        .collect()
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
        sim: r.get(7)?,
        parts: decode_parts(&r.get::<_, String>(8)?),
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
    fn encrypting_and_back_keeps_everything() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("dolphone.db");
        {
            let s = Store::open(&path).unwrap();
            s.add_message("+61491570156", Direction::Incoming, "secret plans", 5, MessageState::Received).unwrap();
        }
        assert!(!is_encrypted(&path));
        let key = [7u8; 32];
        rekey_file(&path, None, Some(&key)).unwrap();
        assert!(is_encrypted(&path));
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(12).any(|w| w == b"secret plans"), "no plain text in the file");
        assert!(Store::open(&path).is_err(), "without the key it does not open");
        assert!(Store::open_keyed(&path, Some(&[8u8; 32])).is_err(), "nor with another");
        let s = Store::open_keyed(&path, Some(&key)).unwrap();
        assert_eq!(s.conversation("+61491570156").unwrap()[0].body, "secret plans");
        let v: i64 = s.db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 4, "the schema version comes along");
        drop(s);
        rekey_file(&path, Some(&key), None).unwrap();
        assert!(!is_encrypted(&path));
        assert_eq!(Store::open(&path).unwrap().threads().unwrap().len(), 1);
    }

    #[test]
    fn mms_parts_are_kept() {
        let s = Store::in_memory().unwrap();
        let id =
            s.add_message("+61491570156,+61491570157", Direction::Incoming, "", 10, MessageState::Received).unwrap();
        let parts = vec![
            Part { mime: "image/jpeg".into(), path: "/data/mms/1-0.jpg".into() },
            Part { mime: "bad\tmime".into(), path: "x".into() },
        ];
        s.set_parts(id, &parts).unwrap();
        let m = s.message(id).unwrap().unwrap();
        assert_eq!(m.parts, parts[..1], "a tab or newline cannot split the stored list");
        assert_eq!(s.threads().unwrap()[0].last_body, "[Picture]");
    }

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
    fn prefix_blocks_and_grants() {
        let s = Store::in_memory().unwrap();
        s.block("1300*").unwrap();
        assert!(s.is_blocked("1300123456").unwrap());
        assert!(!s.is_blocked("1800123456").unwrap());
        let g = s.add_grant("backup.sh", "+61491570156, +6140*", 5, 0).unwrap();
        assert_eq!(s.grant_for("backup.sh", "+61491570156", 10).unwrap(), Some((g, 5)));
        assert_eq!(s.grant_for("backup.sh", "+61401234567", 10).unwrap(), Some((g, 5)));
        assert_eq!(s.grant_for("other", "+61491570156", 10).unwrap(), None);
        let lapsing = s.add_grant("t", "+61491570157", 1, 100).unwrap();
        assert!(s.grant_for("t", "+61491570157", 99).unwrap().is_some());
        assert!(s.grant_for("t", "+61491570157", 100).unwrap().is_none(), "lapsed");
        s.revoke_grant(lapsing).unwrap();
    }

    #[test]
    fn alerts_dedupe_and_audit_caps() {
        let s = Store::in_memory().unwrap();
        assert!(s.add_alert(4370, 0, "Evacuate", 1, 7).unwrap().is_some());
        assert!(s.add_alert(4370, 0, "Evacuate", 2, 7).unwrap().is_none(), "a rebroadcast is the same alert");
        assert_eq!(s.alerts(10).unwrap().len(), 1);
        for i in 0..1100 {
            s.audit("p", "send", "x", "waiting", i).unwrap();
        }
        assert!(s.audit_log(5000).unwrap().len() <= 1001);
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

    #[test]
    fn search_finds_words_and_ignores_syntax() {
        let s = Store::in_memory().unwrap();
        s.add_message(
            "+61491570156",
            Direction::Incoming,
            "Your parcel is at the post office",
            10,
            MessageState::Received,
        )
        .unwrap();
        s.add_message("+61491570157", Direction::Outgoing, "Lunch tomorrow?", 20, MessageState::Sent).unwrap();
        assert_eq!(s.search("parc off", 10).unwrap().len(), 1, "prefixes, all words");
        assert_eq!(s.search("lunch", 10).unwrap()[0].peer, "+61491570157");
        assert!(s.search("\"; DROP TABLE messages; --", 10).unwrap().is_empty(), "the query is words, not SQL or FTS");
        assert!(s.search("NEAR(parcel office)", 10).unwrap().len() <= 1);
        s.delete_thread("+61491570156").unwrap();
        assert!(s.search("parcel", 10).unwrap().is_empty(), "deleted messages leave the index");
    }

    #[test]
    fn sims_are_noted_and_export_is_sms_backup_and_restore() {
        let s = Store::in_memory().unwrap();
        s.set_sim("8961 0001 2345 6789 0123");
        s.add_message(
            "+61491570156",
            Direction::Incoming,
            "Fish & chips <tonight>?",
            1_700_000_000,
            MessageState::Received,
        )
        .unwrap();
        s.add_message("+61491570156", Direction::Outgoing, "Yes \"please\"", 1_700_000_060, MessageState::Failed)
            .unwrap();
        s.add_call("+61491570156", Direction::Incoming, 1_700_000_100, 0, Outcome::Missed).unwrap();
        assert_eq!(s.conversation("+61491570156").unwrap()[0].sim, "0123", "the last four digits");
        assert_eq!(s.recents(1).unwrap()[0].sim, "0123");
        let xml = s.export_xml(|_| "Alex Example".into()).unwrap();
        assert!(xml.starts_with("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n<smses count=\"2\">"));
        assert!(xml.contains("date=\"1700000000000\" type=\"1\""), "milliseconds, received");
        assert!(xml.contains("body=\"Fish &amp; chips &lt;tonight&gt;?\""));
        assert!(xml.contains("type=\"5\"") && xml.contains("status=\"64\""), "a failed text");
        assert!(xml.contains("contact_name=\"Alex Example\""));
        assert!(xml.trim_end().ends_with("</smses>"));
    }
}
