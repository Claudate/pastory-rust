//! Port of `Clipboard/ClipDB.swift` — the SQLite index (WAL).
//!
//! One `items` table, schema verbatim from §5.2 (Swift source, byte for byte
//! in column order). Single-row upsert / delete for everyday edits; `save_all`
//! (whole table in one transaction) for bulk changes. Deleted rows leave a
//! tombstone so an import cannot bring them back.

use objc2_foundation::NSDate;
use rusqlite::Connection;

use crate::clipboard::item::{ClipItem, ClipKind};

pub struct ClipDB {
    conn: Connection,
}

impl ClipDB {
    /// `SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX`,
    /// then busy_timeout and the PRAGMA order the Swift version uses.
    pub fn open(path: &str) -> rusqlite::Result<ClipDB> {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(std::time::Duration::from_millis(3000))?;
        let db = ClipDB { conn };
        db.exec_batch(&[
            "PRAGMA journal_mode=WAL",
            "PRAGMA synchronous=NORMAL",
            "PRAGMA secure_delete=ON", // deleted rows are overwritten with zeros, not just unlinked
        ])?;
        db.exec(
            "CREATE TABLE IF NOT EXISTS items (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_at REAL NOT NULL,
                source_bundle TEXT, source_name TEXT, snippet TEXT NOT NULL, ocr_text TEXT,
                pinned INTEGER NOT NULL DEFAULT 0, ext TEXT NOT NULL, has_rtf INTEGER NOT NULL DEFAULT 0,
                pixel_w INTEGER, pixel_h INTEGER, byte_count INTEGER NOT NULL DEFAULT 0,
                duration REAL, title TEXT, content_hash INTEGER NOT NULL DEFAULT 0
            )",
        )?;
        db.exec("CREATE INDEX IF NOT EXISTS items_created ON items(created_at DESC)")?;
        // Added later: last-modified time (sync merges on it). Old rows get created_at.
        if !db.column_names("items").contains(&"modified_at".to_string()) {
            db.exec("ALTER TABLE items ADD COLUMN modified_at REAL")?;
            db.exec("UPDATE items SET modified_at = created_at WHERE modified_at IS NULL")?;
        }
        // Deleted items leave a marker so an import or a future sync cannot bring them back.
        db.exec(
            "CREATE TABLE IF NOT EXISTS tombstones (id TEXT PRIMARY KEY, content_hash INTEGER NOT NULL, deleted_at REAL NOT NULL)",
        )?;
        Ok(db)
    }

    fn exec(&self, sql: &str) -> rusqlite::Result<()> {
        self.conn.execute_batch(sql)
    }

    fn exec_batch(&self, stmts: &[&str]) -> rusqlite::Result<()> {
        for s in stmts {
            self.exec(s)?;
        }
        Ok(())
    }

    /// `PRAGMA table_info` column names, as Swift's `columnNames`.
    fn column_names(&self, table: &str) -> Vec<String> {
        let sql = format!("PRAGMA table_info({table})");
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
                for name in rows.flatten() {
                    out.push(name);
                }
            }
        }
        out
    }

    // MARK: Tombstones

    pub fn add_tombstones(&self, items: &[ClipItem], at: f64) -> rusqlite::Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT OR REPLACE INTO tombstones (id, content_hash, deleted_at) VALUES (?,?,?)",
        )?;
        for it in items {
            stmt.execute(rusqlite::params![it.id, it.content_hash, at])?;
        }
        Ok(())
    }

    /// Content hashes of everything deleted and not yet purged.
    pub fn tombstone_hashes(&self) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare("SELECT content_hash FROM tombstones")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Fold the write-ahead log back into the main file and truncate it, so page images of deleted rows do not
    /// linger in pastory.sqlite-wal. Called after sweeps and deletes; harmless if nothing is pending.
    pub fn checkpoint(&self) {
        let _ = self.exec("PRAGMA wal_checkpoint(TRUNCATE)");
    }

    pub fn purge_tombstones_before(&self, date: f64) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM tombstones WHERE deleted_at < ?1",
            rusqlite::params![date],
        )?;
        Ok(())
    }

    // MARK: Rows

    /// Newest first.
    pub fn load_all(&self) -> rusqlite::Result<Vec<ClipItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, created_at, source_bundle, source_name, snippet, ocr_text, pinned, ext, has_rtf,
                    pixel_w, pixel_h, byte_count, duration, title, content_hash, modified_at
             FROM items ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let kind_raw: String = r.get(1)?;
            let created: f64 = r.get(2)?;
            let bundle: Option<String> = r.get(3)?;
            let app: Option<String> = r.get(4)?;
            let snippet: String = r.get(5)?;
            let ocr: Option<String> = r.get(6)?;
            let pinned: i64 = r.get(7)?;
            let ext: String = r.get(8)?;
            let has_rtf: i64 = r.get(9)?;
            let pw: Option<i64> = r.get(10)?;
            let ph: Option<i64> = r.get(11)?;
            let bc: Option<i64> = r.get(12)?;
            let dur: Option<f64> = r.get(13)?;
            let title: Option<String> = r.get(14)?;
            let hash: Option<i64> = r.get(15)?;
            let modified: Option<f64> = r.get(16)?;
            Ok((id, kind_raw, created, bundle, app, snippet, ocr, pinned, ext, has_rtf, pw, ph, bc, dur, title, hash, modified))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind_raw, created, bundle, app, snippet, ocr, pinned, ext, has_rtf, pw, ph, bc, dur, title, hash, modified) = row?;
            let Some(kind) = ClipKind::from_raw(&kind_raw) else { continue };
            out.push(ClipItem::new(
                id,
                kind,
                created,
                bundle,
                app,
                snippet,
                ocr,
                pinned != 0,
                ext,
                has_rtf != 0,
                pw,
                ph,
                bc.unwrap_or(0),
                dur,
                title,
                hash.unwrap_or(0),
                modified,
            ));
        }
        Ok(out)
    }

    /// Insert or update one row.
    pub fn upsert(&self, it: &ClipItem) -> rusqlite::Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT OR REPLACE INTO items (id, kind, created_at, source_bundle, source_name, snippet, ocr_text, pinned, ext, has_rtf,
                                           pixel_w, pixel_h, byte_count, duration, title, content_hash, modified_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )?;
        stmt.execute(rusqlite::params![
            it.id,
            it.kind.raw(),
            it.created_at,
            it.source_bundle_id,
            it.source_app_name,
            it.snippet,
            it.ocr_text,
            it.pinned as i64,
            it.ext,
            it.has_rtf as i64,
            it.pixel_width,
            it.pixel_height,
            it.byte_count,
            it.duration,
            it.title,
            it.content_hash,
            it.modified_at,
        ])?;
        Ok(())
    }

    /// Remove one row.
    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM items WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Replace the whole table with `items` atomically.
    pub fn save_all(&self, items: &[ClipItem]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch("DELETE FROM items")?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO items (id, kind, created_at, source_bundle, source_name, snippet, ocr_text, pinned, ext, has_rtf,
                                               pixel_w, pixel_h, byte_count, duration, title, content_hash, modified_at)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )?;
            for it in items {
                stmt.execute(rusqlite::params![
                    it.id,
                    it.kind.raw(),
                    it.created_at,
                    it.source_bundle_id,
                    it.source_app_name,
                    it.snippet,
                    it.ocr_text,
                    it.pinned as i64,
                    it.ext,
                    it.has_rtf as i64,
                    it.pixel_width,
                    it.pixel_height,
                    it.byte_count,
                    it.duration,
                    it.title,
                    it.content_hash,
                    it.modified_at,
                ])?;
            }
        }
        tx.commit()
    }
}

/// `Date()` in seconds since 1970 (used for tombstone stamps).
pub fn now_ts() -> f64 {
    NSDate::now().timeIntervalSince1970()
}
