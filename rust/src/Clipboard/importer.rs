//! Port of `Clipboard/Importer.swift` — the M1 slice.
//!
//! The scan machinery is complete (temp-copy reads, folder walk by SQLite
//! header, the too-broad guard, path trip-wires), and so is the Pastory →
//! Pastory exact path. The two foreign readers land with M6:
//!   - Paste (wiheads) Core Data → ZRAWPASTEBOARDITEMS with LZFSE unwrap
//!   - the generic heuristic scan
//! For now a foreign database answers `Failure::Empty`, which is what the UI
//! already surfaces as "nothing importable here".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

use crate::capture::screenshotter;
use crate::clipboard::item::now;
use crate::clipboard::store::{ImportEntry, ImportPayload};

pub struct Scan {
    pub entries: Vec<ImportEntry>,
    pub tables: Vec<String>,
}

impl Scan {
    pub fn texts(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.payload, ImportPayload::Text(_)))
            .count()
    }

    pub fn images(&self) -> usize {
        self.entries.len() - self.texts()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Failure {
    NotSQLite,
    Empty,
    TooBroad,
}

impl Failure {
    /// `Failure.errorDescription`.
    pub fn message(&self) -> String {
        crate::app::localization::l(match self {
            Failure::NotSQLite => "这不是 SQLite 数据库文件",
            Failure::Empty => "没有找到能导入的文本或图片",
            Failure::TooBroad => "请选择某个剪贴板工具自己的数据文件夹，而不是整个资源库",
        })
    }
}

/// `url` may be a database file (any extension) or a folder: a Pastory store,
/// or any folder that has SQLite files somewhere inside (found by file header,
/// up to three levels down) — all of them are read.
pub fn scan(url: &Path) -> Result<Scan, Failure> {
    if !url.is_dir() {
        return scan_file(url);
    }
    let own = url.join("pastory.sqlite");
    if own.is_file() {
        return scan_file(&own);
    }
    // A whole Library / Application Support / home folder holds every app's
    // databases; that is never what anyone means.
    let path = url
        .canonicalize()
        .unwrap_or_else(|_| url.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME")
        .to_string_lossy()
        .into_owned();
    let too_broad = [
        home.clone(),
        format!("{home}/Library"),
        format!("{home}/Library/Application Support"),
        format!("{home}/Library/Containers"),
        format!("{home}/Library/Group Containers"),
        "/Applications".to_string(),
        "/Users".to_string(),
        "/".to_string(),
    ];
    if too_broad.iter().any(|p| p == &path) {
        return Err(Failure::TooBroad);
    }
    let dbs = sqlite_files_under(url, 3);
    if dbs.len() > 6 {
        return Err(Failure::TooBroad);
    }
    let mut merged = Scan {
        entries: Vec::new(),
        tables: Vec::new(),
    };
    let mut last_error = Failure::NotSQLite;
    for f in dbs {
        match scan_file(&f) {
            Ok(s) => {
                merged.entries.extend(s.entries);
                let name = f
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                merged.tables.extend(s.tables.into_iter().map(|t| format!("{name}:{t}")));
            }
            Err(e) => last_error = e,
        }
    }
    if merged.entries.is_empty() {
        return Err(last_error);
    }
    Ok(merged)
}

fn sqlite_files_under(dir: &Path, depth: i32) -> Vec<PathBuf> {
    if depth < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            out.extend(sqlite_files_under(&path, depth - 1));
            continue;
        }
        let ext = name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        if name.contains('.') && (ext.ends_with("wal") || ext.ends_with("shm")) {
            continue;
        }
        if has_sqlite_header(&path) {
            out.push(path);
        }
    }
    out
}

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

fn has_sqlite_header(path: &Path) -> bool {
    use std::os::unix::fs::FileExt;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 16];
    f.read_exact_at(&mut buf, 0).is_ok() && &buf == SQLITE_HEADER
}

/// Work on a copy (with its WAL/SHM) so a database the other app has open is
/// never touched.
fn scan_file(file: &Path) -> Result<Scan, Failure> {
    if !file.is_file() {
        return Err(Failure::NotSQLite);
    }
    let tmp = std::env::temp_dir().join(format!(
        "pastory-import-{}",
        objc2_foundation::NSUUID::new().UUIDString()
    ));
    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Tmp(tmp.clone());
    std::fs::create_dir_all(&tmp).map_err(|_| Failure::NotSQLite)?;
    let file_name = file
        .file_name()
        .ok_or(Failure::NotSQLite)?
        .to_string_lossy()
        .into_owned();
    let copy = tmp.join(&file_name);
    std::fs::copy(file, &copy).map_err(|_| Failure::NotSQLite)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{}", file.to_string_lossy(), suffix));
        if side.is_file() {
            let _ = std::fs::copy(&side, PathBuf::from(format!("{}{}", copy.to_string_lossy(), suffix)));
        }
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_FULL_MUTEX;
    let conn = Connection::open_with_flags(&copy, flags).map_err(|_| Failure::NotSQLite)?;
    let tables: Vec<String> = query(
        &conn,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .map_err(|_| Failure::NotSQLite)?
    .iter()
    .filter_map(|row| match row.get("name") {
        Some(Value::Text(s)) => Some(s.clone()),
        _ => None,
    })
    .collect();
    if tables.is_empty() {
        return Err(Failure::NotSQLite);
    }
    let items_dir = file
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join("items");
    let is_pastory = tables.iter().any(|t| t == "items")
        && columns(&conn, "items").iter().any(|c| c == "content_hash");
    let is_paste = tables.iter().any(|t| t == "ZITEMENTITY")
        && tables.iter().any(|t| t == "ZITEMDATAENTITY");
    let entries = if is_pastory {
        pastory(&conn, &items_dir).map_err(|_| Failure::NotSQLite)?
    } else if is_paste {
        // Core Data external storage: `.<db>_SUPPORT/_EXTERNAL_DATA` next to
        // the database; the Paste layout reads archives from there when the
        // table blob is a reference.
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let external_dir = file
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join(format!(".{stem}_SUPPORT"))
            .join("_EXTERNAL_DATA");
        let from_paste = paste_reader(&conn, &external_dir).ok().filter(|v| !v.is_empty());
        match from_paste {
            Some(list) => list,
            None => generic(&conn, &tables)?,
        }
    } else {
        generic(&conn, &tables)?
    };
    if entries.is_empty() {
        return Err(Failure::Empty);
    }
    Ok(Scan { entries, tables })
}

// MARK: Pastory → Pastory

fn pastory(conn: &Connection, items_dir: &Path) -> Result<Vec<ImportEntry>, Failure> {
    let rows = query(
        conn,
        "SELECT id, kind, created_at, pinned, ext, title FROM items ORDER BY created_at DESC",
    )
    .map_err(|_| Failure::NotSQLite)?;
    let mut out = Vec::new();
    for row in rows {
        let (Some(Value::Text(id)), Some(Value::Text(kind)), Some(Value::Text(ext))) =
            (row.get("id"), row.get("kind"), row.get("ext"))
        else {
            continue;
        };
        // Stay inside items/.
        if id.contains('/') || id.contains("..") || ext.contains('/') || ext.contains("..") {
            continue;
        }
        let Ok(data) = std::fs::read(items_dir.join(format!("{id}.{ext}"))) else {
            continue;
        };
        let date = match row.get("created_at") {
            Some(Value::Real(r)) => *r,
            Some(Value::Integer(i)) => *i as f64,
            _ => now(),
        };
        let pinned = matches!(row.get("pinned"), Some(Value::Integer(i)) if *i != 0);
        let title = match row.get("title") {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        };
        match kind.as_str() {
            "text" | "url" => {
                if let Ok(s) = String::from_utf8(data) {
                    out.push(ImportEntry {
                        payload: ImportPayload::Text(s),
                        created_at: date,
                        pinned,
                        title,
                    });
                }
            }
            "image" => {
                if let Some(png) = screenshotter::png_data_from_image_bytes(&data) {
                    out.push(ImportEntry {
                        payload: ImportPayload::Image(png),
                        created_at: date,
                        pinned,
                        title,
                    });
                }
            }
            // File lists point at the other machine's paths; recordings are
            // not carried over.
            _ => continue,
        }
    }
    Ok(out)
}

// MARK: SQLite glue

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    query(conn, &format!("PRAGMA table_info(\"{table}\")"))
        .map(|rows| {
            rows.iter()
                .filter_map(|row| match row.get("name") {
                    Some(Value::Text(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn query(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<HashMap<String, Value>>> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let n = names.len();
    let rows = stmt.query_map([], |r| {
        let mut row = HashMap::with_capacity(n);
        for (i, name) in names.iter().enumerate() {
            if let Ok(v) = r.get::<_, Value>(i) {
                row.insert(name.clone(), v);
            }
        }
        Ok(row)
    })?;
    rows.collect()
}
// MARK: Paste (wiheads) — Core Data: ZITEMENTITY → ZITEMDATAENTITY.ZRAWPASTEBOARDITEMS
//
// Lists: ZRAWTYPE 1 = Clipboard History, 2 = a pinboard (seen on a real
// Setapp install). Items on a pinboard are pinned. Without that column,
// fall back to "the biggest list is the history".

fn paste_reader(conn: &Connection, external_dir: &Path) -> Result<Vec<ImportEntry>, Failure> {
    let item_cols = columns(conn, "ZITEMENTITY");
    let data_cols = columns(conn, "ZITEMDATAENTITY");
    if !data_cols.iter().any(|c| c == "ZRAWPASTEBOARDITEMS") || !item_cols.iter().any(|c| c == "Z_PK") {
        return Err(Failure::NotSQLite);
    }
    let mut history_lists: std::collections::HashSet<i64> = query(
        conn,
        "SELECT Z_PK AS pk FROM ZLISTENTITY WHERE ZRAWTYPE = 1",
    )
    .map_err(|_| Failure::NotSQLite)?
    .iter()
    .filter_map(|row| match row.get("pk") {
        Some(Value::Integer(i)) => Some(*i),
        _ => None,
    })
    .collect();
    if history_lists.is_empty() {
        let rows = query(
            conn,
            "SELECT ZLIST AS l, COUNT(*) AS n FROM ZITEMENTITY GROUP BY ZLIST",
        )
        .map_err(|_| Failure::NotSQLite)?;
        let big = rows.iter().max_by_key(|row| match row.get("n") {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        });
        if let Some(big) = big {
            if let Some(Value::Integer(l)) = big.get("l") {
                history_lists.insert(*l);
            }
        }
    }
    let external = ExternalStore::new(external_dir);
    let col = |name: &str, have: &[String]| -> String {
        if have.iter().any(|c| c == name) {
            name.to_string()
        } else {
            "NULL".to_string()
        }
    };
    let sql = format!(
        "SELECT Z_PK AS pk, {} AS created, {} AS ts, {} AS list, {} AS data\n         FROM ZITEMENTITY ORDER BY COALESCE({}, {}) DESC",
        col("ZCREATEDAT", &item_cols),
        col("ZTIMESTAMP", &item_cols),
        col("ZLIST", &item_cols),
        col("ZDATA", &item_cols),
        col("ZTIMESTAMP", &item_cols),
        col("ZCREATEDAT", &item_cols),
    );
    let rows = query(conn, &sql).map_err(|_| Failure::NotSQLite)?;
    // Data rows point at the item (ZITEM) and the item points at its data
    // (ZDATA); either may be missing.
    let mut by_item: HashMap<i64, Vec<Vec<u8>>> = HashMap::new();
    let mut by_pk: HashMap<i64, Vec<u8>> = HashMap::new();
    let data_sql = format!(
        "SELECT Z_PK AS pk, {} AS item, ZRAWPASTEBOARDITEMS AS blob FROM ZITEMDATAENTITY",
        col("ZITEM", &data_cols)
    );
    for row in query(conn, &data_sql).map_err(|_| Failure::NotSQLite)? {
        let Some(Value::Blob(blob)) = row.get("blob") else { continue };
        if let Some(Value::Integer(item)) = row.get("item") {
            by_item.entry(*item).or_default().push(blob.clone());
        }
        if let Some(Value::Integer(pk)) = row.get("pk") {
            by_pk.insert(*pk, blob.clone());
        }
    }
    let mut out = Vec::new();
    for row in rows {
        let Some(Value::Integer(pk)) = row.get("pk") else { continue };
        let mut candidates: Vec<Vec<u8>> = by_item.get(pk).cloned().unwrap_or_default();
        if let Some(Value::Integer(data_pk)) = row.get("data") {
            if let Some(b) = by_pk.get(data_pk) {
                if !candidates.contains(b) {
                    candidates.push(b.clone());
                }
            }
        }
        let mut payload: Option<ImportPayload> = None;
        for blob in candidates {
            let resolved = external.resolve(&blob).unwrap_or(blob);
            if let Some(p) = pasteboard_archive::payload(&pasteboard_archive::unwrap(&resolved)) {
                payload = Some(p);
                break;
            }
        }
        let Some(payload) = payload else { continue };
        let date = row
            .get("ts")
            .or_else(|| row.get("created"))
            .and_then(as_date)
            .unwrap_or_else(now);
        let pinned = match row.get("list") {
            Some(Value::Integer(l)) => !history_lists.contains(l),
            _ => false,
        };
        out.push(ImportEntry {
            payload,
            created_at: date,
            pinned,
            title: None, // Paste's ZTITLE is an auto preview, not a name
        });
    }
    Ok(out)
}

/// Core Data "allows external storage": the column holds a small reference,
/// the bytes live in `.<db>_SUPPORT/_EXTERNAL_DATA/<UUID>`. The reference
/// format is undocumented, so match the UUID (as text or as raw 16 bytes)
/// against the files that actually exist.
struct ExternalStore {
    dir: PathBuf,
    names: std::collections::HashSet<String>,
}

impl ExternalStore {
    fn new(dir: &Path) -> ExternalStore {
        let names = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        ExternalStore {
            dir: dir.to_path_buf(),
            names,
        }
    }

    fn resolve(&self, ref_blob: &[u8]) -> Option<Vec<u8>> {
        if self.names.is_empty() || ref_blob.len() > 512 {
            return None;
        }
        if let Ok(s) = std::str::from_utf8(ref_blob) {
            for name in &self.names {
                if s.contains(name.as_str()) {
                    if let Ok(bytes) = std::fs::read(self.dir.join(name)) {
                        return Some(bytes);
                    }
                }
            }
        }
        if ref_blob.len() >= 16 {
            for i in 0..=(ref_blob.len() - 16) {
                let u = uuid_string(&ref_blob[i..i + 16]);
                for candidate in [&u, &u.to_lowercase()] {
                    if self.names.contains(candidate.as_str()) {
                        if let Ok(bytes) = std::fs::read(self.dir.join(candidate)) {
                            return Some(bytes);
                        }
                    }
                }
            }
        }
        None
    }
}

/// Foundation UUID: 8-4-4-4-12 hex groups, raw bytes per RFC 4122.
fn uuid_string(bytes: &[u8]) -> String {
    debug_assert!(bytes.len() == 16);
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        hex[0], hex[1], hex[2], hex[3],
        hex[4], hex[5],
        hex[6], hex[7],
        hex[8], hex[9],
        hex[10], hex[11], hex[12], hex[13], hex[14], hex[15],
    )
}
// MARK: Anything else (generic heuristic)

/// `Importer.generic` — every tool's database that is neither Pastory nor
/// Paste. One entry per source item: rows sharing a parent are the same clip
/// in several flavours (plain text + rtf + html, png + tiff). Keep one image,
/// else the longest plain text.
fn generic(conn: &Connection, tables: &[String]) -> Result<Vec<ImportEntry>, Failure> {
    let mut out = Vec::new();
    let skip: std::collections::HashSet<&str> =
        ["Z_METADATA", "Z_PRIMARYKEY", "Z_MODELCACHE"].into_iter().collect();
    // Tables that can lend a date to a child row (Core Data: ZHISTORYITEM for
    // ZHISTORYITEMCONTENT).
    let mut parents: Vec<(String, String, Option<String>)> = Vec::new();
    for t in tables {
        let cols = columns(conn, t);
        if !cols.iter().any(|c| c == "Z_PK") {
            continue;
        }
        if let Some(d) = cols.iter().find(|c| is_date_column(c)) {
            let pin = cols.iter().find(|c| is_pin_column(c)).cloned();
            parents.push((t.clone(), d.clone(), pin));
        }
    }
    let mut used_as_parent: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Content tables first; a table that turned out to be somebody's parent
    // holds titles and metadata, not payloads.
    let mut ordered: Vec<&String> = tables.iter().filter(|t| !skip.contains(t.as_str())).collect();
    ordered.sort_by_key(|t| parents.iter().any(|p| p.0 == **t));
    for t in ordered {
        if used_as_parent.contains(t.as_str()) {
            continue;
        }
        let cols = columns(conn, t);
        let date_col = cols.iter().find(|c| is_date_column(c)).cloned();
        let pin_col = cols.iter().find(|c| is_pin_column(c)).cloned();
        let type_col = cols.iter().find(|c| {
            let n = c.to_lowercase();
            n.contains("type") || n.contains("uti") || n == "kind"
        });
        // Which integer column points at a parent row that has the date?
        let mut join: Option<(String, (String, String, Option<String>))> = None;
        if date_col.is_none() {
            let total = query(conn, &format!("SELECT COUNT(*) AS n FROM \"{t}\""))
                .ok()
                .and_then(|rows| match rows.first()?.get("n") {
                    Some(Value::Integer(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0);
            if total > 0 {
                'outer: for c in &cols {
                    if ["Z_PK", "Z_ENT", "Z_OPT"].contains(&c.as_str()) {
                        continue;
                    }
                    for p in &parents {
                        if &p.0 == t {
                            continue;
                        }
                        let hit = query(
                            conn,
                            &format!("SELECT COUNT(*) AS n FROM \"{t}\" WHERE \"{c}\" IN (SELECT Z_PK FROM \"{}\")", p.0),
                        )
                        .ok()
                        .and_then(|rows| match rows.first()?.get("n") {
                            Some(Value::Integer(n)) => Some(*n),
                            _ => None,
                        })
                        .unwrap_or(0);
                        if hit * 2 > total {
                            join = Some((c.clone(), p.clone()));
                            break 'outer;
                        }
                    }
                }
            }
        }
        let mut sql = "SELECT c.* ".to_string();
        if let Some((_, parent)) = &join {
            sql += &format!(", p.\"{}\" AS __pdate", parent.1);
            if let Some(pin) = &parent.2 {
                sql += &format!(", p.\"{pin}\" AS __ppin");
            }
        }
        sql += &format!(" FROM \"{t}\" c");
        if let Some((join_col, parent)) = &join {
            sql += &format!(" LEFT JOIN \"{}\" p ON c.\"{join_col}\" = p.Z_PK", parent.0);
        }
        sql += " LIMIT 50000";
        let Ok(rows) = query(conn, &sql) else {
            continue;
        };
        #[derive(Default)]
        struct Candidate {
            text: Option<String>,
            image: Option<Vec<u8>>,
            png_image: bool,
            date: f64,
            pinned: bool,
        }
        let mut groups: HashMap<i64, Candidate> = HashMap::new();
        let mut order: Vec<i64> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let hint = type_col
                .and_then(|c| row.get(c))
                .and_then(|v| match v {
                    Value::Text(s) => Some(s.to_lowercase()),
                    _ => None,
                })
                .unwrap_or_default();
            if hint.contains("rtf")
                || hint.contains("html")
                || hint.contains("file-url")
                || hint.starts_with("dyn.")
                || hint.contains("filename")
            {
                continue;
            }
            let date = date_col
                .as_ref()
                .and_then(|c| row.get(c))
                .and_then(as_date)
                .or_else(|| row.get("__pdate").and_then(as_date))
                .unwrap_or_else(now);
            let pinned = pin_col
                .as_ref()
                .and_then(|c| row.get(c))
                .map(truthy)
                .unwrap_or(false)
                || row.get("__ppin").map(truthy).unwrap_or(false);
            let mut best: Option<String> = None;
            let mut image: Option<Vec<u8>> = None;
            for c in &cols {
                if type_col.map(|tc| tc == c).unwrap_or(false) {
                    continue;
                }
                if pin_col.as_ref().map(|pc| pc == c).unwrap_or(false) || is_date_column(c) {
                    continue;
                }
                let lower = c.to_lowercase();
                if lower.ends_with("id")
                    || lower == "uuid"
                    || lower.contains("bundle")
                    || lower.contains("source")
                    || lower.contains("app")
                    || lower.contains("title")
                    || lower.contains("name")
                    || lower.contains("label")
                {
                    continue;
                }
                match row.get(c) {
                    Some(Value::Text(s)) => {
                        if !looks_like_identifier(s) && s.len() > best.as_ref().map(|b| b.len()).unwrap_or(0) {
                            best = Some(s.clone());
                        }
                    }
                    Some(Value::Blob(d)) => {
                        if let Some(png) = screenshotter::png_data_from_image_bytes(d) {
                            image = Some(png);
                        } else if hint.is_empty()
                            || hint.contains("text")
                            || hint.contains("utf8")
                            || hint.contains("string")
                            || hint.contains("url")
                        {
                            if let Ok(s) = String::from_utf8(d.clone()) {
                                if !s.is_empty() && s.len() > best.as_ref().map(|b| b.len()).unwrap_or(0) {
                                    best = Some(s);
                                }
                            }
                        }
                    }
                    _ => continue,
                }
            }
            if image.is_none() && best.is_none() {
                continue;
            }
            let key: i64 = join
                .as_ref()
                .and_then(|(join_col, _)| match row.get(join_col) {
                    Some(Value::Integer(pk)) => Some(*pk),
                    _ => None,
                })
                .unwrap_or(-1 - i as i64);
            let entry = match groups.entry(key) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    order.push(key);
                    v.insert(Candidate::default())
                }
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            };
            if let Some(img) = image {
                if !entry.png_image {
                    entry.png_image = hint.contains("png");
                    entry.image = Some(img);
                }
            }
            if let Some(t) = best {
                if t.len() > entry.text.as_ref().map(|e| e.len()).unwrap_or(0) {
                    entry.text = Some(t);
                }
            }
            if entry.date == 0.0 {
                entry.date = date;
            }
            entry.pinned = entry.pinned || pinned;
        }
        for key in order {
            let Some(g) = groups.get(&key) else { continue };
            if let Some(img) = &g.image {
                out.push(ImportEntry {
                    payload: ImportPayload::Image(img.clone()),
                    created_at: g.date,
                    pinned: g.pinned,
                    title: None,
                });
            } else if let Some(t) = &g.text {
                let trimmed = t.trim();
                if !trimmed.is_empty() && t.len() <= 200_000 {
                    out.push(ImportEntry {
                        payload: ImportPayload::Text(t.clone()),
                        created_at: g.date,
                        pinned: g.pinned,
                        title: None,
                    });
                }
            }
        }
        if let Some((_, parent)) = &join {
            used_as_parent.insert(parent.0.clone());
        }
    }
    Ok(out)
}

// MARK: Heuristics

fn is_date_column(c: &str) -> bool {
    let n = c.to_lowercase();
    n.contains("date")
        || n.contains("time")
        || n.contains("created")
        || n.contains("copied")
        || n.contains("updated")
        || n.ends_with("_at")
}

/// Core Data object URIs, UUIDs, hashes: the kind of string a database is
/// full of and nobody ever copied.
fn looks_like_identifier(s: &str) -> bool {
    let t = s.trim();
    if t.starts_with("x-coredata://") {
        return true;
    }
    // UUID in its 36-byte text form.
    if t.len() == 36
        && t.chars().enumerate().all(|(i, c)| {
            c.is_ascii_hexdigit() || ([8, 13, 18, 23].contains(&i) && c == '-')
        })
    {
        return true;
    }
    if (16..=128).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return true;
    }
    false
}

/// The column has to *be* a pin flag, not merely contain "pin"
/// (typing, shipping, mapping…).
fn is_pin_column(c: &str) -> bool {
    let mut n = c.to_lowercase();
    if n.starts_with('z') {
        n.remove(0);
    }
    if let Some(rest) = n.strip_prefix("is_") {
        n = rest.to_string();
    } else if let Some(rest) = n.strip_prefix("is") {
        n = rest.to_string();
    }
    ["pin", "pinned", "favorite", "favourite", "favorited", "starred", "star"]
        .contains(&n.as_str())
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Integer(i) => *i != 0,
        Value::Real(d) => *d != 0.0,
        Value::Text(s) => ["1", "true", "yes"].contains(&s.to_lowercase().as_str()),
        _ => false,
    }
}

/// Seconds since 1970, since 2001 (Core Data), or milliseconds. Swift also
/// accepts ISO-8601 text (Foundation's parser); real tools store numeric
/// epochs, and the heuristic reads fall back to `Date()` on anything else.
fn as_date(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Real(d) => *d,
        Value::Integer(i) => *i as f64,
        Value::Text(s) => s.parse().ok()?,
        _ => return None,
    };
    if n <= 0.0 {
        return None;
    }
    if n > 1e12 {
        return Some(n / 1000.0);
    }
    if n > 1.2e9 {
        return Some(n);
    }
    if n > 3e8 {
        // Reference date: 2001-01-01 00:00:00 UTC = 978307200 s after 1970.
        return Some(n + 978307200.0);
    }
    None
}
// MARK: PasteboardArchive — serialized pasteboard items

/// Serialized pasteboard items (Paste stores an archive of [UTI: bytes]).
/// Keyed archive, plain plist, or raw bytes — the archive walker pulls one
/// picture or one piece of text.
///
/// LZFSE: decoded through the **system Compression.framework**
/// (`compression_decode_buffer`, COMPRESSION_LZFSE ↔ bvx… frames,
/// COMPRESSION_ZLIB ↔ the raw-deflate rows) — the chosen alternative over the
/// `lzfse` crate: Foundation's framework is the same algorithm Paste's own
/// export uses, adds no third-party dependency (§3.2 allows "或系统
/// Compression.framework"), and links as a system dylib.
mod pasteboard_archive {
    use crate::capture::screenshotter;
    use crate::clipboard::store::ImportPayload;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};

    /// Paste wraps the archive: first byte 0x01, then the plist compressed
    /// (LZFSE-framed "bvx…" for some rows, raw deflate for others); 0x02
    /// rows are external-file references and are resolved before we get here.
    pub fn unwrap(blob: &[u8]) -> Vec<u8> {
        if blob.len() <= 2 {
            return blob.to_vec();
        }
        let body = if blob[0] == 0x01 { &blob[1..] } else { blob };
        if body.starts_with(b"bplist") {
            return body.to_vec();
        }
        let order: &[objc2_foundation::NSDataCompressionAlgorithm] = if body.starts_with(b"bvx") {
            &[COMPRESSION_LZFSE]
        } else {
            &[COMPRESSION_ZLIB, COMPRESSION_LZFSE, COMPRESSION_LZ4, COMPRESSION_LZMA]
        };
        for i in 0..order.len() {
            if let Some(out) = decompress(order[i], body) {
                if out.len() <= (64 << 20) && (out.starts_with(b"bplist") || out.first() == Some(&b'<')) {
                    return out;
                }
            }
        }
        blob.to_vec()
    }

    /// Pull out one picture or one piece of text.
    pub fn payload(blob: &[u8]) -> Option<ImportPayload> {
        let mut found: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(obj) = decode(blob) {
            collect(&obj, &mut found, "");
        }
        // Preference: an image, else plain text, else a URL string.
        for (uti, d) in &found {
            if is_image_uti(uti) {
                if let Some(png) = screenshotter::png_data_from_image_bytes(d) {
                    return Some(ImportPayload::Image(png));
                }
            }
        }
        for (uti, d) in &found {
            if uti.contains("utf8-plain-text") || uti == "public.plain-text" || uti.ends_with("string") {
                if let Some(s) = String::from_utf8(d.clone()).ok().or_else(|| utf16_string(d)) {
                    if !s.is_empty() {
                        return Some(ImportPayload::Text(s));
                    }
                }
            }
        }
        for (uti, d) in &found {
            if uti.contains("url") && !uti.contains("file") {
                if let Ok(s) = String::from_utf8(d.clone()) {
                    if !s.is_empty() {
                        return Some(ImportPayload::Text(s));
                    }
                }
            }
        }
        // Raw fallbacks: a bare picture, or bare text.
        if let Some(png) = screenshotter::png_data_from_image_bytes(blob) {
            return Some(ImportPayload::Image(png));
        }
        if blob.len() < 200_000 {
            if let Ok(s) = String::from_utf8(blob.to_vec()) {
                if !s.is_empty()
                    && s.chars().all(|c| c as u32 >= 0x20 || c == '\n' || c == '\t' || c == '\r')
                {
                    return Some(ImportPayload::Text(s));
                }
            }
        }
        None
    }

    fn utf16_string(d: &[u8]) -> Option<String> {
        if d.len() % 2 != 0 || d.is_empty() {
            return None;
        }
        let units: Vec<u16> = d
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).ok()
    }

    #[derive(Debug)]
    enum Obj {
        Data(Vec<u8>),
        Text(String),
        Dict(Vec<(String, Obj)>),
        Array(Vec<Obj>),
    }

    fn decode(blob: &[u8]) -> Option<Obj> {
        if !blob.starts_with(b"bplist") && blob.first() != Some(&b'<') {
            return None;
        }
        let ns_data = NSData::from_vec(blob.to_vec());
        if let Some(obj) = try_unarchive(&ns_data) {
            return Some(obj);
        }
        try_plist(&ns_data)
    }

    /// NSKeyedUnarchiver with the same class allow-list as Swift.
    fn try_unarchive(ns_data: &NSData) -> Option<Obj> {
        let classes: [&AnyClass; 7] = [
            objc2::class!(NSArray),
            objc2::class!(NSDictionary),
            objc2::class!(NSString),
            objc2::class!(NSData),
            objc2::class!(NSURL),
            objc2::class!(NSNumber),
            objc2::class!(NSDate),
        ];
        let set: Retained<objc2_foundation::NSSet<AnyObject>> = unsafe {
            let arr = NSArray::from_slice(&classes);
            let raw: *mut objc2_foundation::NSSet<AnyObject> = msg_send![
                objc2::class!(NSSet),
                setWithArray: &*arr
            ];
            Retained::from_raw(raw)?
        };
        let obj: Option<Retained<AnyObject>> = unsafe {
            let unarchiver = objc2::class!(NSKeyedUnarchiver);
            let mut err: *mut objc2_foundation::NSError = std::ptr::null_mut();
            msg_send![
                unarchiver,
                unarchivedObjectOfClasses: &*set,
                fromData: ns_data,
                error: &mut err
            ]
        };
        obj.map(|o| from_foundation(&o))
    }

    /// NSPropertyListSerialization.propertyListWithData:options:format:error:
    fn try_plist(ns_data: &NSData) -> Option<Obj> {
        let ns = objc2::class!(NSPropertyListSerialization);
        let mut format: usize = 0;
        let mut err: *mut objc2_foundation::NSError = std::ptr::null_mut();
        let obj: Option<Retained<AnyObject>> = unsafe {
            msg_send![
                ns,
                propertyListWithData: ns_data,
                options: 0usize,
                format: &mut format,
                error: &mut err
            ]
        };
        obj.map(|o| from_foundation(&o))
    }

    fn from_foundation(obj: &AnyObject) -> Obj {
        let class = obj.class();
        let name = class.name().to_bytes();
        let is = |n: &[u8]| name.starts_with(n);
        if is(b"NSDictionary") || is(b"__NSDictionary") || is(b"__NSCFDictionary") {
            let dict: &NSDictionary<NSString, AnyObject> = unsafe {
                &*(obj as *const AnyObject as *const NSDictionary<NSString, AnyObject>)
            };
            let mut pairs = Vec::new();
            let keys = dict.allKeys();
            // Every key is guaranteed to be in the dictionary, so the
            // placeholder never surfaces; still give one per the signature.
            let placeholder: &AnyObject = &*NSObject::new();
            let objects = dict.objectsForKeys_notFoundMarker(&keys, placeholder);
            for (k, v) in keys.iter().zip(objects.iter()) {
                pairs.push((k.to_string(), from_foundation(&v)));
            }
            return Obj::Dict(pairs);
        }
        if is(b"NSArray") || is(b"__NSArray") || is(b"__NSCFArray") {
            let arr: &NSArray<AnyObject> = unsafe {
                &*(obj as *const AnyObject as *const NSArray<AnyObject>)
            };
            return Obj::Array(arr.iter().map(|v| from_foundation(&v)).collect());
        }
        if is(b"NSData") || is(b"__NSData") || is(b"__NSCFData") || is(b"NSConcreteData") {
            let d: &NSData = unsafe { &*(obj as *const AnyObject as *const NSData) };
            return Obj::Data(unsafe { d.as_bytes_unchecked() }.to_vec());
        }
        if is(b"NSURL") || is(b"NSFileReferenceURL") {
            let s = unsafe {
                let s: Retained<NSString> = msg_send![obj, absoluteString];
                s.to_string()
            };
            return Obj::Text(s);
        }
        if is(b"NSString") || is(b"__NSCFString") || is(b"NSTaggedPointerString") || is(b"NSMutableString") {
            let s: &NSString = unsafe { &*(obj as *const AnyObject as *const NSString) };
            return Obj::Text(s.to_string());
        }
        if is(b"NSNumber") || is(b"__NSCFNumber") || is(b"__NSCFBoolean") {
            let s = unsafe {
                let s: Retained<NSString> = msg_send![obj, stringValue];
                s.to_string()
            };
            return Obj::Text(s);
        }
        if is(b"NSDate") || is(b"__NSDate") || is(b"__NSCFDate") {
            let secs: f64 = unsafe { msg_send![obj, timeIntervalSince1970] };
            return Obj::Text(secs.to_string());
        }
        Obj::Text(String::new())
    }

    fn collect(obj: &Obj, out: &mut Vec<(String, Vec<u8>)>, key: &str) {
        match obj {
            Obj::Data(d) => {
                if !key.is_empty() {
                    out.push((key.to_string(), d.clone()));
                } else if let Some(inner) = decode(d) {
                    collect(&inner, out, "");
                }
            }
            Obj::Text(s) => {
                if key.contains("text") || key.contains("string") || key.contains("url") {
                    out.push((key.to_string(), s.as_bytes().to_vec()));
                }
            }
            Obj::Dict(pairs) => {
                let find = |names: &[&str]| -> Option<&Obj> {
                    names
                        .iter()
                        .find_map(|n| pairs.iter().find(|(k, _)| k == n))
                        .map(|(_, v)| v)
                };
                if let Some(Obj::Text(t)) = find(&["type", "uti", "typeIdentifier"]) {
                    let t = t.to_lowercase();
                    if let Some(v) = find(&["data", "value", "bytes"]) {
                        collect(v, out, &t);
                        return;
                    }
                }
                if let Some(Obj::Array(types_arr)) = find(&["types"]) {
                    let types: Vec<String> = types_arr
                        .iter()
                        .filter_map(|o| match o {
                            Obj::Text(s) => Some(s.to_lowercase()),
                            _ => None,
                        })
                        .collect();
                    // "types" next to a "data…" key (the Paste shape).
                    if let Some((_, datas)) = pairs.iter().find(|(k, _)| k.to_lowercase().starts_with("data")) {
                        match datas {
                            Obj::Array(arr) if arr.len() == types.len() => {
                                for (t, v) in types.iter().zip(arr.iter()) {
                                    collect(v, out, t);
                                }
                            }
                            Obj::Dict(by_type) => {
                                for t in &types {
                                    if let Some((_, v)) = by_type.iter().find(|(k, _)| k == t) {
                                        collect(v, out, t);
                                    }
                                }
                            }
                            _ => {
                                if let Some(first) = types.first() {
                                    collect(datas, out, first);
                                }
                            }
                        }
                    } else if let Some(first) = types.first() {
                        collect(&Obj::Text(String::new()), out, first);
                    }
                    return;
                }
                for (k, v) in pairs {
                    if k.starts_with('$') {
                        continue;
                    }
                    let next = if k.contains('.') { k.to_lowercase() } else { key.to_string() };
                    collect(v, out, &next);
                }
                if let Some(Obj::Array(objects)) = find(&["$objects"]) {
                    for o in objects {
                        collect(o, out, "");
                    }
                }
            }
            Obj::Array(items) => {
                for v in items {
                    collect(v, out, key);
                }
            }
        }
    }

    fn is_image_uti(u: &str) -> bool {
        u.contains("png")
            || u.contains("tiff")
            || u.contains("jpeg")
            || u.contains("jpg")
            || u.contains("heic")
    }

    // MARK: Decompression via NSData (Foundation wires Compression.framework)

    /// `NSData.decompressed(using:)` through the generated Foundation
    /// bindings — the app's four algorithm enum values in the same order
    /// Swift tries (lzfse | zlib | lz4 | lzma).
    pub const COMPRESSION_LZFSE: objc2_foundation::NSDataCompressionAlgorithm =
        objc2_foundation::NSDataCompressionAlgorithm::LZFSE;
    pub const COMPRESSION_ZLIB: objc2_foundation::NSDataCompressionAlgorithm =
        objc2_foundation::NSDataCompressionAlgorithm::Zlib;
    pub const COMPRESSION_LZ4: objc2_foundation::NSDataCompressionAlgorithm =
        objc2_foundation::NSDataCompressionAlgorithm::LZ4;
    pub const COMPRESSION_LZMA: objc2_foundation::NSDataCompressionAlgorithm =
        objc2_foundation::NSDataCompressionAlgorithm::LZMA;

    fn decompress(algo: objc2_foundation::NSDataCompressionAlgorithm, body: &[u8]) -> Option<Vec<u8>> {
        let data = NSData::from_vec(body.to_vec());
        let out: Option<Retained<NSData>> = unsafe {
            let mut error: *mut objc2_foundation::NSError = std::ptr::null_mut();
            msg_send![&*data, decompressedUsingAlgorithm: algo, error: &mut error]
        };
        out.map(|d| unsafe { d.as_bytes_unchecked() }.to_vec())
    }
}
