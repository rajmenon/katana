//! Persistent clipboard history with consecutive-copy dedup.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    pub id: i64,
    pub kind: String,
    pub hash: String,
    pub text: Option<String>,
    pub pinned: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Read width/height from a PNG IHDR without decoding pixels.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    if &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

const CLIP_SELECT: &str = "SELECT id, kind, hash, text, pinned, width, height FROM clips";

fn clip_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClipItem> {
    Ok(ClipItem {
        id: r.get(0)?,
        kind: r.get(1)?,
        hash: r.get(2)?,
        text: r.get(3)?,
        pinned: r.get::<_, i64>(4)? != 0,
        width: r.get::<_, Option<i64>>(5)?.and_then(|n| u32::try_from(n).ok()),
        height: r.get::<_, Option<i64>>(6)?.and_then(|n| u32::try_from(n).ok()),
    })
}

pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Consecutive identical copies are skipped.
pub fn should_record(prev_hash: Option<&str>, new_hash: &str) -> bool {
    prev_hash != Some(new_hash)
}

pub struct ClipStore {
    conn: Connection,
}

impl ClipStore {
    pub fn open(path: &Path) -> Result<Self, ClipError> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS clips (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                hash TEXT NOT NULL,
                text TEXT,
                image BLOB,
                pinned INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                width INTEGER,
                height INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_clips_hash ON clips(hash);
            ",
        )?;
        let _ = conn.execute("ALTER TABLE clips ADD COLUMN width INTEGER", []);
        let _ = conn.execute("ALTER TABLE clips ADD COLUMN height INTEGER", []);
        Ok(Self { conn })
    }

    pub fn last_hash(&self) -> Result<Option<String>, ClipError> {
        let h = self
            .conn
            .query_row(
                "SELECT hash FROM clips ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(h)
    }

    pub fn record_text(&self, text: &str) -> Result<Option<i64>, ClipError> {
        let hash = content_hash(text.as_bytes());
        let prev = self.last_hash()?;
        if !should_record(prev.as_deref(), &hash) {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO clips (kind, hash, text) VALUES ('text', ?1, ?2)",
            params![hash, text],
        )?;
        Ok(Some(self.conn.last_insert_rowid()))
    }

    pub fn record_image(&self, bytes: &[u8]) -> Result<Option<i64>, ClipError> {
        let hash = content_hash(bytes);
        let prev = self.last_hash()?;
        if !should_record(prev.as_deref(), &hash) {
            return Ok(None);
        }
        let (width, height) = match png_size(bytes) {
            Some((w, h)) => (Some(w as i64), Some(h as i64)),
            None => (None, None),
        };
        self.conn.execute(
            "INSERT INTO clips (kind, hash, image, width, height) VALUES ('image', ?1, ?2, ?3, ?4)",
            params![hash, bytes, width, height],
        )?;
        Ok(Some(self.conn.last_insert_rowid()))
    }

    pub fn count(&self) -> Result<usize, ClipError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Drop oldest unpinned clips until `max` remain. Pinned clips are kept
    /// even if that means the store is briefly over the cap.
    pub fn trim_to(&self, max: usize) -> Result<usize, ClipError> {
        let max = max.max(1) as i64;
        let total = self.count()? as i64;
        if total <= max {
            return Ok(0);
        }
        let excess = total - max;
        let n = self.conn.execute(
            "DELETE FROM clips WHERE id IN (
                SELECT id FROM clips WHERE pinned = 0 ORDER BY id ASC LIMIT ?1
             )",
            params![excess],
        )?;
        Ok(n)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<ClipItem>, ClipError> {
        let mut stmt = self.conn.prepare(&format!(
            "{CLIP_SELECT}
             ORDER BY pinned DESC, id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit as i64], clip_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn search(&self, q: &str, limit: usize) -> Result<Vec<ClipItem>, ClipError> {
        let ql = q.trim().to_ascii_lowercase();
        if matches!(ql.as_str(), "image" | "img" | "png" | "picture") {
            return self.list_kind("image", limit);
        }
        let like = format!("%{q}%");
        let mut stmt = self.conn.prepare(&format!(
            "{CLIP_SELECT}
             WHERE text LIKE ?1
             ORDER BY pinned DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![like, limit as i64], clip_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn list_kind(&self, kind: &str, limit: usize) -> Result<Vec<ClipItem>, ClipError> {
        let mut stmt = self.conn.prepare(&format!(
            "{CLIP_SELECT}
             WHERE kind = ?1
             ORDER BY pinned DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![kind, limit as i64], clip_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn pin(&self, id: i64, pinned: bool) -> Result<(), ClipError> {
        self.conn.execute(
            "UPDATE clips SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?;
        Ok(())
    }

    pub fn get_image(&self, id: i64) -> Result<Option<Vec<u8>>, ClipError> {
        let b = self
            .conn
            .query_row("SELECT image FROM clips WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(b)
    }

    pub fn delete(&self, id: i64) -> Result<bool, ClipError> {
        let n = self
            .conn
            .execute("DELETE FROM clips WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    pub fn clear_all(&self) -> Result<usize, ClipError> {
        let n = self.conn.execute("DELETE FROM clips", [])?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "katana-clip-{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn consecutive_dedup_and_persist_across_reopen() {
        let path = tmp();
        let id1;
        let img_id;
        {
            let s = ClipStore::open(&path).unwrap();
            id1 = s.record_text("hello").unwrap().expect("first insert");
            assert!(s.record_text("hello").unwrap().is_none(), "consecutive dup");
            assert!(s.record_text("world").unwrap().is_some());
            img_id = s
                .record_image(&[0x89, b'P', b'N', b'G'])
                .unwrap()
                .expect("image");
            s.pin(id1, true).unwrap();
        }
        let s = ClipStore::open(&path).unwrap();
        let all = s.list(20).unwrap();
        assert!(
            all.iter().any(|c| c.text.as_deref() == Some("hello") && c.pinned),
            "text+pin must survive restart: {all:?}"
        );
        assert!(all.iter().any(|c| c.text.as_deref() == Some("world")));
        assert_eq!(s.get_image(img_id).unwrap().unwrap(), [0x89, b'P', b'N', b'G']);
        let found = s.search("wor", 10).unwrap();
        assert_eq!(found[0].text.as_deref(), Some("world"));
        let img = all.iter().find(|c| c.id == img_id).unwrap();
        assert!(img.width.is_none() && img.height.is_none(), "truncated PNG has no IHDR");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn should_record_compares_hashes() {
        let h = content_hash(b"abc");
        assert!(!should_record(Some(&h), &h));
        assert!(should_record(Some(&h), &content_hash(b"xyz")));
        assert!(should_record(None, &h));
    }

    #[test]
    fn delete_one_and_clear_all() {
        let path = tmp();
        let s = ClipStore::open(&path).unwrap();
        let a = s.record_text("keep").unwrap().unwrap();
        let b = s.record_text("gone").unwrap().unwrap();
        assert!(s.delete(b).unwrap());
        let left = s.list(10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, a);
        assert_eq!(s.clear_all().unwrap(), 1);
        assert!(s.list(10).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        let mut p = Vec::from(*b"\x89PNG\r\n\x1a\n");
        p.extend_from_slice(&13u32.to_be_bytes());
        p.extend_from_slice(b"IHDR");
        p.extend_from_slice(&w.to_be_bytes());
        p.extend_from_slice(&h.to_be_bytes());
        p.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        p
    }

    #[test]
    fn png_size_reads_ihdr() {
        assert_eq!(png_size(&tiny_png(1920, 1080)), Some((1920, 1080)));
        assert!(png_size(&[0x89, b'P', b'N', b'G']).is_none());
    }

    #[test]
    fn trim_drops_oldest_unpinned_keeps_pinned() {
        let path = tmp();
        let s = ClipStore::open(&path).unwrap();
        let a = s.record_text("a").unwrap().unwrap();
        let b = s.record_text("b").unwrap().unwrap();
        let c = s.record_text("c").unwrap().unwrap();
        let d = s.record_text("d").unwrap().unwrap();
        s.pin(a, true).unwrap();
        s.pin(c, true).unwrap();
        assert_eq!(s.trim_to(3).unwrap(), 1);
        let left: Vec<i64> = s.list(10).unwrap().into_iter().map(|x| x.id).collect();
        assert!(left.contains(&a) && left.contains(&c), "pinned must remain: {left:?}");
        assert!(left.contains(&d), "newest unpinned kept: {left:?}");
        assert!(!left.contains(&b), "oldest unpinned dropped: {left:?}");
        assert_eq!(s.count().unwrap(), 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn trim_never_deletes_pinned_even_over_cap() {
        let path = tmp();
        let s = ClipStore::open(&path).unwrap();
        let a = s.record_text("pin-a").unwrap().unwrap();
        let b = s.record_text("pin-b").unwrap().unwrap();
        s.pin(a, true).unwrap();
        s.pin(b, true).unwrap();
        assert_eq!(s.trim_to(1).unwrap(), 0);
        assert_eq!(s.count().unwrap(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn record_image_stores_png_dimensions() {
        let path = tmp();
        let s = ClipStore::open(&path).unwrap();
        let png = tiny_png(64, 32);
        s.record_image(&png).unwrap();
        let img = s.list(1).unwrap().into_iter().next().unwrap();
        assert_eq!((img.width, img.height), (Some(64), Some(32)));
        let _ = std::fs::remove_file(path);
    }
}
