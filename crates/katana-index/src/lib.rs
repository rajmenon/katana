//! In-memory filename index. Search never walks the tree.

use std::path::{Path, PathBuf};

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use katana_core::FuzzyEngine;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("privilege or journal lost; rebuild required")]
    NeedsRebuild,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileHit {
    pub name: String,
    pub path: String,
    pub score: f32,
    pub size: Option<u64>,
    pub is_dir: bool,
    pub type_name: String,
}

/// Explorer-style type column.
pub fn type_name_of(name: &str, is_dir: bool) -> String {
    if is_dir {
        return "File folder".into();
    }
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "" => "File".into(),
        "txt" => "Text Document".into(),
        "md" => "Markdown File".into(),
        "pdf" => "PDF File".into(),
        "doc" | "docx" => "Microsoft Word Document".into(),
        "xls" | "xlsx" => "Microsoft Excel Worksheet".into(),
        "ppt" | "pptx" => "Microsoft PowerPoint Presentation".into(),
        "jpg" | "jpeg" => "JPEG Image".into(),
        "png" => "PNG Image".into(),
        "gif" => "GIF Image".into(),
        "webp" => "WEBP Image".into(),
        "svg" => "SVG File".into(),
        "mp3" | "wav" | "flac" => "Audio".into(),
        "mp4" | "mkv" | "webm" => "Video".into(),
        "zip" | "7z" | "rar" => "Compressed Folder".into(),
        "exe" => "Application".into(),
        "dll" => "Application extension".into(),
        "rs" => "Rust Source".into(),
        "py" => "Python File".into(),
        "js" | "ts" => "JavaScript File".into(),
        "json" => "JSON File".into(),
        "toml" => "TOML File".into(),
        "html" | "htm" => "HTML File".into(),
        "css" => "CSS File".into(),
        other => format!("{} File", other.to_ascii_uppercase()),
    }
}

/// Explorer-like Date modified (`2026-09-02 13:04`).
pub fn format_mtime(unix_secs: u64) -> String {
    let days = unix_secs / 86400;
    let rem = unix_secs % 86400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    // Civil from days since 1970-01-01 (Howard Hinnant).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{min:02}")
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if (bytes as f64) < MB {
        format!("{:.1} KB", bytes as f64 / KB)
    } else if (bytes as f64) < GB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{:.2} GB", bytes as f64 / GB)
    }
}

/// Compact SoA: names and paths interned in two arenas.
#[derive(Debug, Default)]
pub struct NameIndex {
    names: Vec<u8>,
    paths: Vec<u8>,
    entries: Vec<Entry>,
    stale: bool,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    name_off: u32,
    name_len: u16,
    path_off: u32,
    path_len: u32,
    size: u64,
    flags: u16,
}

impl NameIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn mark_rebuild(&mut self) {
        self.stale = true;
    }

    pub fn needs_rebuild(&self) -> bool {
        self.stale
    }

    pub fn clear_rebuild(&mut self) {
        self.stale = false;
    }

    /// Record a journal wrap / lost privilege. Search still works but
    /// [`needs_rebuild`] is true so callers rescan instead of serving stale as live.
    pub fn on_journal_wrap(&mut self) {
        self.mark_rebuild();
    }

    pub fn remove_path(&mut self, path: &str) {
        let keep: Vec<Entry> = self
            .entries
            .iter()
            .copied()
            .filter(|e| self.path_at(e) != path)
            .collect();
        self.entries = keep;
    }

    pub fn insert(&mut self, name: &str, path: &str) {
        self.insert_full(name, path, None, false);
    }

    pub fn insert_full(&mut self, name: &str, path: &str, size: Option<u64>, is_dir: bool) {
        if name.is_empty() || path.is_empty() {
            return;
        }
        let name_b = name.as_bytes();
        let path_b = path.as_bytes();
        let name_len = name_b.len().min(u16::MAX as usize);
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(&name_b[..name_len]);
        let path_off = self.paths.len() as u32;
        self.paths.extend_from_slice(path_b);
        self.entries.push(Entry {
            name_off,
            name_len: name_len as u16,
            path_off,
            path_len: path_b.len() as u32,
            size: size.unwrap_or(0),
            flags: if is_dir { 1 } else { 0 } | if size.is_some() { 2 } else { 0 },
        });
    }

    fn slice_utf8(buf: &[u8], off: u32, len: u32) -> &str {
        let s = off as usize;
        let e = s.saturating_add(len as usize);
        if s > buf.len() || e > buf.len() || s > e {
            return "";
        }
        std::str::from_utf8(&buf[s..e]).unwrap_or("")
    }

    fn name_at(&self, e: &Entry) -> &str {
        Self::slice_utf8(&self.names, e.name_off, e.name_len as u32)
    }

    fn path_at(&self, e: &Entry) -> &str {
        Self::slice_utf8(&self.paths, e.path_off, e.path_len)
    }

    fn to_hit(&self, e: &Entry, score: f32) -> FileHit {
        let name = self.name_at(e).to_string();
        let is_dir = e.flags & 1 != 0;
        let size = if e.flags & 2 != 0 { Some(e.size) } else { None };
        FileHit {
            type_name: type_name_of(&name, is_dir),
            name,
            path: self.path_at(e).to_string(),
            score,
            size,
            is_dir,
        }
    }

    /// Search names (and paths for glob/regex). Never walks the tree.
    /// Only the top `limit` hits are materialized — 1-char queries must not
    /// allocate a hit per file.
    pub fn search(&self, query: &str, limit: usize) -> Vec<FileHit> {
        let q = query.trim();
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        let kind = parse_file_query(q);
        let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        let push = |heap: &mut BinaryHeap<Reverse<(u32, u32)>>, score: f32, idx: u32| {
            let key = (score * 10_000.0) as u32;
            if heap.len() < limit {
                heap.push(Reverse((key, idx)));
            } else if let Some(Reverse((worst, _))) = heap.peek() {
                if key > *worst {
                    heap.pop();
                    heap.push(Reverse((key, idx)));
                }
            }
        };
        match &kind {
            FileQuery::Fuzzy(needle) => {
                let mut eng = FuzzyEngine::new();
                let pat = FuzzyEngine::prepare(needle);
                let nl = needle.to_ascii_lowercase();
                for (i, e) in self.entries.iter().enumerate() {
                    let name = self.name_at(e);
                    if name.is_empty() {
                        continue;
                    }
                    let score = if name.eq_ignore_ascii_case(needle) {
                        Some(1.0)
                    } else if name.to_ascii_lowercase().starts_with(&nl) {
                        Some(0.96)
                    } else {
                        eng.score_prepared(&pat, name)
                    };
                    if let Some(score) = score {
                        push(&mut heap, score, i as u32);
                    }
                }
            }
            FileQuery::Wildcard(pat) => {
                for (i, e) in self.entries.iter().enumerate() {
                    let name = self.name_at(e);
                    let path = self.path_at(e);
                    if wildcard_match(pat, name) || wildcard_match(pat, path) {
                        push(&mut heap, 1.0, i as u32);
                    }
                }
            }
            FileQuery::Regex(pat) => {
                if let Ok(re) = regex::RegexBuilder::new(pat)
                    .case_insensitive(true)
                    .build()
                {
                    for (i, e) in self.entries.iter().enumerate() {
                        let name = self.name_at(e);
                        let path = self.path_at(e);
                        if re.is_match(name) || re.is_match(path) {
                            push(&mut heap, 1.0, i as u32);
                        }
                    }
                }
            }
        }
        let mut ranked: Vec<(u32, u32)> = heap.into_iter().map(|Reverse(v)| v).collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        ranked
            .into_iter()
            .filter_map(|(key, idx)| {
                self.entries
                    .get(idx as usize)
                    .map(|e| self.to_hit(e, key as f32 / 10_000.0))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileQuery {
    Fuzzy(String),
    Wildcard(String),
    Regex(String),
}

/// `re:foo.*` or `/foo.*/` → regex; `*.pdf` / `inv?ice.*` → wildcard; else fuzzy.
pub fn parse_file_query(q: &str) -> FileQuery {
    let q = q.trim();
    if let Some(rest) = q.strip_prefix("re:") {
        return FileQuery::Regex(rest.to_string());
    }
    if q.len() >= 3 && q.starts_with('/') && q.ends_with('/') {
        return FileQuery::Regex(q[1..q.len() - 1].to_string());
    }
    if q.contains('*') || q.contains('?') {
        return FileQuery::Wildcard(q.to_string());
    }
    FileQuery::Fuzzy(q.to_string())
}

pub fn wildcard_match(pat: &str, text: &str) -> bool {
    let p = pat.to_ascii_lowercase();
    let n = text.to_ascii_lowercase();
    wildcard_bytes(p.as_bytes(), n.as_bytes())
}

fn wildcard_bytes(p: &[u8], n: &[u8]) -> bool {
    let (mut i, mut j) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, j));
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

/// One MFT record used to reconstruct parent paths (testable without NTFS).
#[derive(Debug, Clone)]
pub struct MftRow {
    pub fid: u64,
    pub parent_fid: u64,
    pub name: String,
    pub is_dir: bool,
}

/// Rebuild `C:\dir\file` paths from FRN + parent FRN (not `C:\filename`).
pub fn rebuild_paths(letter: char, rows: &[MftRow]) -> NameIndex {
    use std::collections::HashMap;
    let by_fid: HashMap<u64, &MftRow> = rows.iter().map(|r| (r.fid, r)).collect();
    let mut idx = NameIndex::new();
    for r in rows {
        if r.name.is_empty() || r.name == "." {
            continue;
        }
        let path = resolve_mft_path(letter, r, &by_fid);
        idx.insert_full(&r.name, &path, None, r.is_dir);
    }
    idx
}

fn resolve_mft_path(letter: char, row: &MftRow, by_fid: &std::collections::HashMap<u64, &MftRow>) -> String {
    let mut parts: Vec<&str> = vec![row.name.as_str()];
    let mut cur = row.parent_fid;
    let start = row.fid;
    for _ in 0..64 {
        if cur == 0 || cur == start {
            break;
        }
        let Some(parent) = by_fid.get(&cur) else {
            break;
        };
        if parent.fid == parent.parent_fid || parent.name == "." || parent.name.is_empty() {
            break;
        }
        parts.push(parent.name.as_str());
        if parent.parent_fid == parent.fid {
            break;
        }
        cur = parent.parent_fid;
    }
    parts.reverse();
    format!("{letter}:\\{}", parts.join("\\"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnCursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

/// Journal wrap or new journal id → must rebuild; never treat as live.
pub fn journal_needs_rebuild(cursor: &UsnCursor, journal_id: u64, first_usn: i64) -> bool {
    cursor.journal_id != 0 && (journal_id != cursor.journal_id || cursor.next_usn < first_usn)
}

/// Point the cursor at the live journal without enumerating history.
#[cfg(windows)]
pub fn open_usn_cursor(letter: char) -> Result<UsnCursor, IndexError> {
    use usn_journal_rs::volume::Volume;
    let volume = Volume::from_drive_letter(letter)
        .map_err(|e| IndexError::Msg(format!("volume {letter}: {e}")))?;
    let data = volume
        .journal()
        .query(false)
        .map_err(|e| IndexError::Msg(e.to_string()))?;
    Ok(UsnCursor {
        journal_id: data.journal_id,
        next_usn: data.next_usn,
    })
}

#[cfg(not(windows))]
pub fn open_usn_cursor(_letter: char) -> Result<UsnCursor, IndexError> {
    Err(IndexError::NeedsRebuild)
}

#[derive(Debug, Clone)]
pub enum UsnChange {
    Create { name: String, path: String },
    Delete { path: String },
    Rename { old_path: String, new_name: String, new_path: String },
}

impl NameIndex {
    pub fn apply_usn(&mut self, change: UsnChange) {
        match change {
            UsnChange::Create { name, path } => self.insert(&name, &path),
            UsnChange::Delete { path } => self.remove_path(&path),
            UsnChange::Rename {
                old_path,
                new_name,
                new_path,
            } => {
                self.remove_path(&old_path);
                self.insert(&new_name, &new_path);
            }
        }
    }
}

/// Build the live index: MFT+path walk, else user-folder walk.
pub fn build_live_index(user_roots: &[PathBuf]) -> NameIndex {
    let mut idx = {
        #[cfg(windows)]
        {
            if let Some(letter) = boot_drive_letter() {
                if let Ok(idx) = try_mft_volume(letter) {
                    idx
                } else {
                    walk_roots(user_roots)
                }
            } else {
                walk_roots(user_roots)
            }
        }
        #[cfg(not(windows))]
        {
            walk_roots(user_roots)
        }
    };
    inject_folder_roots(&mut idx, user_roots);
    idx
}

fn inject_folder_roots(idx: &mut NameIndex, roots: &[PathBuf]) {
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
            idx.insert_full(name, &root.to_string_lossy(), None, true);
        }
    }
}

fn boot_drive_letter() -> Option<char> {
    std::env::var("SystemDrive")
        .ok()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_uppercase())
}

/// Walk `roots` once and fill an index. Subsequent search does not walk.
pub fn walk_roots(roots: &[PathBuf]) -> NameIndex {
    let mut idx = NameIndex::new();
    for root in roots {
        if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
            if root.is_dir() {
                idx.insert_full(name, &root.to_string_lossy(), None, true);
            }
        }
        walk_into(&mut idx, root);
    }
    idx
}

fn walk_into(idx: &mut NameIndex, root: &Path) {
    walk_into_depth(idx, root, 0);
}

fn walk_into_depth(idx: &mut NameIndex, root: &Path, depth: u8) {
    if depth > 10 {
        return;
    }
    let walker = match std::fs::read_dir(root) {
        Ok(w) => w,
        Err(_) => return,
    };
    for ent in walker.flatten() {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let meta = ent.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.and_then(|m| if is_dir { None } else { Some(m.len()) });
        idx.insert_full(&name, &path.to_string_lossy(), size, is_dir);
        if is_dir && !skip_dir(&name) {
            walk_into_depth(idx, &path, depth + 1);
        }
    }
}

pub fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | "target"
            | "AppData"
            | "Windows"
            | "$Recycle.Bin"
            | "System Volume Information"
            | "anaconda3"
            | ".cargo"
            | ".rustup"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".mypy_cache"
            | "dist"
            | ".next"
            | ".cache"
            | "coverage"
            | ".tox"
            | "site-packages"
            | "hf_voxtral_tmp"
            | "mistral_common_tmp"
            | "mistral_inference_tmp"
            | "voxmlx_tmp"
    )
}

/// Try NTFS MFT enumeration. On privilege failure the caller walks user folders.
#[cfg(windows)]
pub fn try_mft_volume(letter: char) -> Result<NameIndex, IndexError> {
    use usn_journal_rs::volume::Volume;
    let volume = Volume::from_drive_letter(letter).map_err(|e| {
        IndexError::Msg(format!("volume {letter}: {e}"))
    })?;
    let mft = volume.mft();
    let mut rows = Vec::new();
    for rec in mft.iter() {
        match rec {
            Ok(entry) => {
                let name = entry.file_name.to_string_lossy().into_owned();
                if name.is_empty() {
                    continue;
                }
                rows.push(MftRow {
                    fid: entry.fid,
                    parent_fid: entry.parent_fid,
                    name,
                    is_dir: entry.is_dir(),
                });
                if rows.len() >= 2_000_000 {
                    break;
                }
            }
            Err(_) => {
                return Err(IndexError::NeedsRebuild);
            }
        }
    }
    Ok(rebuild_paths(letter, &rows))
}

#[cfg(not(windows))]
pub fn try_mft_volume(_letter: char) -> Result<NameIndex, IndexError> {
    Err(IndexError::NeedsRebuild)
}

/// One USN poll. On wrap/lost privilege marks `idx` stale and returns `NeedsRebuild`.
#[cfg(windows)]
pub fn poll_usn(
    letter: char,
    cursor: &mut UsnCursor,
    idx: &mut NameIndex,
) -> Result<u32, IndexError> {
    use usn_journal_rs::journal::EnumOptions;
    use usn_journal_rs::volume::Volume;
    let volume = Volume::from_drive_letter(letter)
        .map_err(|e| IndexError::Msg(format!("volume {letter}: {e}")))?;
    let journal = volume.journal();
    let data = journal
        .query(false)
        .map_err(|e| IndexError::Msg(e.to_string()))?;
    if journal_needs_rebuild(cursor, data.journal_id, data.first_usn) {
        idx.on_journal_wrap();
        return Err(IndexError::NeedsRebuild);
    }
    let opts = EnumOptions {
        start_usn: cursor.next_usn,
        ..EnumOptions::default()
    };
    let iter = journal
        .iter_with_options(opts)
        .map_err(|e| IndexError::Msg(e.to_string()))?;
    let mut n = 0u32;
    const CREATE: u32 = 0x0000_0100;
    const DELETE: u32 = 0x0000_0200;
    const RENAME_NEW: u32 = 0x0000_2000;
    for rec in iter {
        let entry = match rec {
            Ok(e) => e,
            Err(_) => {
                idx.on_journal_wrap();
                return Err(IndexError::NeedsRebuild);
            }
        };
        cursor.next_usn = entry.usn.saturating_add(1);
        let name = entry.file_name.to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        let path = format!("{letter}:\\{name}");
        if entry.reason & DELETE != 0 {
            idx.apply_usn(UsnChange::Delete { path });
        } else if entry.reason & (CREATE | RENAME_NEW) != 0 {
            idx.apply_usn(UsnChange::Create { name, path });
        }
        n += 1;
        if n > 50_000 {
            break;
        }
    }
    Ok(n)
}

#[cfg(not(windows))]
pub fn poll_usn(
    _letter: char,
    _cursor: &mut UsnCursor,
    idx: &mut NameIndex,
) -> Result<u32, IndexError> {
    idx.on_journal_wrap();
    Err(IndexError::NeedsRebuild)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn search_does_not_walk_and_finds_inserted_name() {
        let mut idx = NameIndex::new();
        idx.insert("invoice.pdf", r"C:\docs\invoice.pdf");
        idx.insert("notes.txt", r"C:\docs\notes.txt");
        idx.insert("InvoiceOld.doc", r"C:\docs\InvoiceOld.doc");
        let hits = idx.search("invoice", 10);
        assert!(
            hits.iter().any(|h| h.name == "invoice.pdf"),
            "shipped search must return inserted filename, got {hits:?}"
        );
        assert!(hits.iter().all(|h| h.path.contains("docs")));
    }

    #[test]
    fn journal_wrap_sets_rebuild_flag() {
        let mut idx = NameIndex::new();
        idx.insert("a.txt", r"C:\a.txt");
        assert!(!idx.needs_rebuild());
        idx.on_journal_wrap();
        assert!(idx.needs_rebuild());
        // still searchable, but not silently "fresh"
        assert_eq!(idx.search("a", 1)[0].name, "a.txt");
    }

    #[test]
    fn mft_rows_rebuild_parent_paths_not_bare_name() {
        let rows = vec![
            MftRow {
                fid: 5,
                parent_fid: 5,
                name: ".".into(),
                is_dir: true,
            },
            MftRow {
                fid: 10,
                parent_fid: 5,
                name: "Users".into(),
                is_dir: true,
            },
            MftRow {
                fid: 20,
                parent_fid: 10,
                name: "rajiv".into(),
                is_dir: true,
            },
            MftRow {
                fid: 30,
                parent_fid: 20,
                name: "invoice.pdf".into(),
                is_dir: false,
            },
        ];
        let idx = rebuild_paths('C', &rows);
        let hits = idx.search("invoice", 4);
        assert_eq!(hits[0].name, "invoice.pdf");
        assert_eq!(
            hits[0].path, r"C:\Users\rajiv\invoice.pdf",
            "must reconstruct parents, not C:\\invoice.pdf"
        );
    }

    #[test]
    fn usn_wrap_and_apply_create_delete() {
        let mut idx = NameIndex::new();
        idx.insert("a.txt", r"C:\a.txt");
        let cur = UsnCursor {
            journal_id: 1,
            next_usn: 100,
        };
        assert!(journal_needs_rebuild(&cur, 2, 100));
        assert!(journal_needs_rebuild(&cur, 1, 200));
        assert!(!journal_needs_rebuild(&cur, 1, 50));
        idx.apply_usn(UsnChange::Create {
            name: "b.txt".into(),
            path: r"C:\b.txt".into(),
        });
        assert!(idx.search("b", 2).iter().any(|h| h.path == r"C:\b.txt"));
        idx.apply_usn(UsnChange::Delete {
            path: r"C:\a.txt".into(),
        });
        assert!(idx.search("a", 2).is_empty());
        assert!(!journal_needs_rebuild(
            &UsnCursor {
                journal_id: 0,
                next_usn: 0
            },
            9,
            1
        ));
    }

    #[test]
    fn search_keeps_only_top_limit_without_losing_best() {
        let mut idx = NameIndex::new();
        for i in 0..400 {
            idx.insert(&format!("file{i:03}.txt"), &format!(r"C:\t\file{i:03}.txt"));
        }
        idx.insert("invoice.pdf", r"C:\docs\invoice.pdf");
        let hits = idx.search("invoice", 3);
        assert!(hits.len() <= 3);
        assert_eq!(hits[0].name, "invoice.pdf");
        assert!(skip_dir("node_modules") && skip_dir("target") && !skip_dir("Documents"));
    }

    #[test]
    fn wildcard_and_regex_and_type_size() {
        let mut idx = NameIndex::new();
        idx.insert_full("invoice.pdf", r"C:\docs\invoice.pdf", Some(2400), false);
        idx.insert_full("notes.txt", r"C:\docs\notes.txt", Some(80), false);
        idx.insert_full("Invoices", r"C:\docs\Invoices", None, true);
        assert_eq!(parse_file_query("*.pdf"), FileQuery::Wildcard("*.pdf".into()));
        assert_eq!(parse_file_query("re:inv.*"), FileQuery::Regex("inv.*".into()));
        assert_eq!(parse_file_query("/inv.*pdf/"), FileQuery::Regex("inv.*pdf".into()));
        let w = idx.search("*.pdf", 10);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].name, "invoice.pdf");
        assert_eq!(w[0].type_name, "PDF File");
        assert_eq!(w[0].size, Some(2400));
        assert_eq!(format_size(2400), "2.3 KB");
        assert_eq!(format_mtime(0), "1970-01-01 00:00");
        assert_eq!(format_mtime(1_704_067_200), "2024-01-01 00:00");
        let r = idx.search("re:note.*", 10);
        assert_eq!(r[0].name, "notes.txt");
        assert!(wildcard_match("inv?ice.*", "invoice.pdf"));
        assert!(!wildcard_match("*.txt", "invoice.pdf"));
        let folder = idx.search("Invoices", 4);
        assert!(folder[0].is_dir);
        assert_eq!(folder[0].type_name, "File folder");
    }

    #[test]
    fn walk_roots_indexes_then_search_is_memory_only() {
        let dir = std::env::temp_dir().join(format!(
            "katana-idx-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("alpha-widget.txt"), b"x").unwrap();
        let idx = walk_roots(&[dir.clone()]);
        // delete file so a walk would miss it — search must still hit the snapshot
        fs::remove_file(dir.join("alpha-widget.txt")).unwrap();
        let hits = idx.search("widget", 8);
        assert!(
            hits.iter().any(|h| h.name == "alpha-widget.txt"),
            "index must answer from memory, not re-walk; {hits:?}"
        );
        let _ = fs::remove_dir_all(dir);
    }
}
