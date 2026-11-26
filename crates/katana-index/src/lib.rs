//! In-memory filename index. Search never walks the tree.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

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
    /// Unix seconds, last write time. `None` when the index never saw it.
    pub modified: Option<u64>,
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

/// Last-write time as unix seconds, if the clock is after the epoch.
pub fn mtime_unix(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Explorer-like Date modified (`2026-09-02 13:04`), UTC.
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
    modified: u64,
    flags: u16,
}

const F_DIR: u16 = 1;
const F_SIZE: u16 = 2;
const F_MTIME: u16 = 4;

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
        self.insert_meta(name, path, size, is_dir, None);
    }

    pub fn insert_meta(
        &mut self,
        name: &str,
        path: &str,
        size: Option<u64>,
        is_dir: bool,
        modified: Option<u64>,
    ) {
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
        let mut flags = 0u16;
        if is_dir {
            flags |= F_DIR;
        }
        if size.is_some() {
            flags |= F_SIZE;
        }
        if modified.is_some() {
            flags |= F_MTIME;
        }
        self.entries.push(Entry {
            name_off,
            name_len: name_len as u16,
            path_off,
            path_len: path_b.len() as u32,
            size: size.unwrap_or(0),
            modified: modified.unwrap_or(0),
            flags,
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
        let is_dir = e.flags & F_DIR != 0;
        let size = if e.flags & F_SIZE != 0 {
            Some(e.size)
        } else {
            None
        };
        let modified = if e.flags & F_MTIME != 0 {
            Some(e.modified)
        } else {
            None
        };
        FileHit {
            type_name: type_name_of(&name, is_dir),
            name,
            path: self.path_at(e).to_string(),
            score,
            size,
            is_dir,
            modified,
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
                // Substring/prefix only. Nucleo over every name stalls the UI
                // once the index holds a volume's worth of files.
                let nl = needle.to_ascii_lowercase();
                let stem = launch_stem(&nl);
                for (i, e) in self.entries.iter().enumerate() {
                    let name = self.name_at(e);
                    // A stem hit (`kestrel` for `kestrel.exe`) stays below an exact
                    // filename so the binary outranks the folder and `kestrel.css`.
                    let score = name_match_score(name, nl.as_bytes()).or_else(|| {
                        stem.and_then(|s| name_match_score(name, s).map(|sc| sc * 0.97))
                    });
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
        let mut seen = HashSet::new();
        ranked
            .into_iter()
            .filter_map(|(key, idx)| {
                self.entries
                    .get(idx as usize)
                    .map(|e| self.to_hit(e, key as f32 / 10_000.0))
            })
            .filter(|h| seen.insert(dedupe_path(&h.path)))
            .collect()
    }
}

/// `terminal.exe` → `terminal`, so a typed exe name still matches `WindowsTerminal.exe`.
fn launch_stem(needle_lower: &str) -> Option<&[u8]> {
    let stem = needle_lower.strip_suffix(".exe").or_else(|| needle_lower.strip_suffix(".lnk"))?;
    if stem.len() < 2 || stem.len() == needle_lower.len() {
        None
    } else {
        Some(stem.as_bytes())
    }
}

fn name_match_score(name: &str, needle_lower: &[u8]) -> Option<f32> {
    if needle_lower.is_empty() || name.is_empty() {
        return None;
    }
    if eq_ignore_ascii(name, needle_lower) {
        Some(1.0)
    } else if starts_ignore_ascii(name, needle_lower) {
        Some(0.96)
    } else if contains_ignore_ascii(name, needle_lower) {
        Some(0.84)
    } else {
        None
    }
}

fn eq_ignore_ascii(hay: &str, needle_lower: &[u8]) -> bool {
    let h = hay.as_bytes();
    h.len() == needle_lower.len()
        && h.iter()
            .zip(needle_lower)
            .all(|(hb, nb)| hb.to_ascii_lowercase() == *nb)
}

fn starts_ignore_ascii(hay: &str, needle_lower: &[u8]) -> bool {
    let h = hay.as_bytes();
    h.len() >= needle_lower.len()
        && h.iter()
            .zip(needle_lower)
            .all(|(hb, nb)| hb.to_ascii_lowercase() == *nb)
}

fn contains_ignore_ascii(hay: &str, needle_lower: &[u8]) -> bool {
    let h = hay.as_bytes();
    if needle_lower.is_empty() || needle_lower.len() > h.len() {
        return needle_lower.is_empty();
    }
    h.windows(needle_lower.len())
        .any(|w| w.iter().zip(needle_lower).all(|(hb, nb)| hb.to_ascii_lowercase() == *nb))
}

fn dedupe_path(path: &str) -> String {
    let p = path.trim();
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    let p = p.strip_prefix(r"\??\").unwrap_or(p);
    let mut s = String::with_capacity(p.len());
    for c in p.chars() {
        if c == '/' {
            s.push('\\');
        } else {
            s.extend(c.to_lowercase());
        }
    }
    while s.ends_with('\\') {
        s.pop();
    }
    s
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
    build_live_index_with(user_roots, &IndexOptions::default())
}

/// Same as [`build_live_index`], using `opts` for the folder walk.
///
/// A successful NTFS MFT read still returns the volume (capped at two million
/// records) and does not apply `opts`. Without that privilege the walk is the
/// index, and it follows `opts` exactly.
pub fn build_live_index_with(user_roots: &[PathBuf], opts: &IndexOptions) -> NameIndex {
    let mut idx = {
        #[cfg(windows)]
        {
            if let Some(letter) = boot_drive_letter() {
                if let Ok(idx) = try_mft_volume(letter) {
                    idx
                } else {
                    walk_roots_with(user_roots, opts)
                }
            } else {
                walk_roots_with(user_roots, opts)
            }
        }
        #[cfg(not(windows))]
        {
            walk_roots_with(user_roots, opts)
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
            let modified = std::fs::metadata(root).ok().as_ref().and_then(mtime_unix);
            idx.insert_meta(name, &root.to_string_lossy(), None, true, modified);
        }
    }
}

fn boot_drive_letter() -> Option<char> {
    std::env::var("SystemDrive")
        .ok()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_uppercase())
}

/// How deep [`walk_roots`] goes under each root. Overridable via settings.
pub const DEFAULT_INDEX_MAX_DEPTH: u8 = 24;

/// What the folder walk indexes. File extensions are never a filter.
///
/// `skip_dirs` is matched case-insensitively against directory names. A name
/// on that list is recorded, but the walk does not enter it. `target` is not
/// a default, so `target/release/*.exe` under an indexed root is searchable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOptions {
    pub skip_dirs: Vec<String>,
    pub index_dotfiles: bool,
    pub max_depth: u8,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            skip_dirs: default_index_skip_dirs(),
            index_dotfiles: true,
            max_depth: DEFAULT_INDEX_MAX_DEPTH,
        }
    }
}

/// Documented default exclusions for `/f`. Not `target`, `dist`, or `*.exe`.
pub fn default_index_skip_dirs() -> Vec<String> {
    [
        "node_modules",
        ".git",
        "AppData",
        "Windows",
        "$Recycle.Bin",
        "System Volume Information",
        "anaconda3",
        ".cargo",
        ".rustup",
        "__pycache__",
        ".venv",
        "venv",
        ".mypy_cache",
        ".next",
        ".cache",
        "coverage",
        ".tox",
        "site-packages",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// True when `name` is a directory the file walk should not enter.
pub fn dir_excluded(name: &str, skip_dirs: &[String]) -> bool {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    skip_dirs.iter().any(|s| {
        let s = s.trim();
        !s.is_empty() && s.eq_ignore_ascii_case(name)
    })
}

/// Drop a root whose files are already reached by walking an ancestor.
/// Home + Documents would otherwise index every document twice.
pub fn prepare_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    prepare_roots_with(roots, &IndexOptions::default())
}

pub fn prepare_roots_with(roots: &[PathBuf], opts: &IndexOptions) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = roots.iter().filter(|p| p.is_dir()).cloned().collect();
    v.sort_by_key(|p| p.components().count());
    let mut kept: Vec<PathBuf> = Vec::new();
    for r in v {
        if kept.iter().any(|k| root_covered(k.as_path(), &r, opts)) {
            continue;
        }
        kept.push(r);
    }
    kept
}

fn root_covered(ancestor: &Path, child: &Path, opts: &IndexOptions) -> bool {
    let Ok(rel) = child.strip_prefix(ancestor) else {
        return false;
    };
    if rel.as_os_str().is_empty() {
        return false;
    }
    let comps: Vec<_> = rel.components().collect();
    // The ancestor walk stops after `max_depth`. A deeper extra root is not covered.
    if comps.is_empty() || comps.len() >= opts.max_depth as usize {
        return false;
    }
    for c in &comps {
        let Some(name) = c.as_os_str().to_str() else {
            return false;
        };
        if dir_excluded(name, &opts.skip_dirs) || (!opts.index_dotfiles && name.starts_with('.')) {
            return false;
        }
    }
    true
}

/// Walk `roots` once and fill an index. Subsequent search does not walk.
pub fn walk_roots(roots: &[PathBuf]) -> NameIndex {
    walk_roots_with(roots, &IndexOptions::default())
}

/// Walk `roots` with explicit exclusions, dotfile policy, and depth.
pub fn walk_roots_with(roots: &[PathBuf], opts: &IndexOptions) -> NameIndex {
    let mut idx = NameIndex::new();
    for root in prepare_roots_with(roots, opts) {
        if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
            let modified = std::fs::metadata(&root).ok().as_ref().and_then(mtime_unix);
            idx.insert_meta(name, &root.to_string_lossy(), None, true, modified);
        }
        walk_into(&mut idx, &root, opts);
    }
    idx
}

fn walk_into(idx: &mut NameIndex, root: &Path, opts: &IndexOptions) {
    walk_into_depth(idx, root, 0, opts);
}

fn walk_into_depth(idx: &mut NameIndex, root: &Path, depth: u8, opts: &IndexOptions) {
    if depth > opts.max_depth {
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
        if !opts.index_dotfiles && name.starts_with('.') {
            continue;
        }
        let meta = ent.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let modified = meta.as_ref().and_then(mtime_unix);
        let size = meta.and_then(|m| if is_dir { None } else { Some(m.len()) });
        idx.insert_meta(&name, &path.to_string_lossy(), size, is_dir, modified);
        if is_dir && !dir_excluded(&name, &opts.skip_dirs) {
            walk_into_depth(idx, &path, depth + 1, opts);
        }
    }
}

/// Directory names the program scanner does not enter (vendor trees and build
/// output). File search does not use this list; it uses [`IndexOptions`].
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
        let dated = hits.iter().find(|h| h.name == "alpha-widget.txt").unwrap();
        assert!(dated.modified.is_some(), "walk must store last-write time");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_paths_collapse_and_nested_roots_walk_once() {
        let mut idx = NameIndex::new();
        idx.insert_meta("a.txt", r"C:\docs\a.txt", Some(3), false, Some(50));
        idx.insert_meta("a.txt", r"C:\docs\a.txt", Some(3), false, Some(50));
        let hits = idx.search("a.txt", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].modified, Some(50));

        let parent = std::env::temp_dir().join(format!(
            "katana-roots-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let child = parent.join("docs");
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("only-once.txt"), b"z").unwrap();
        let once = walk_roots(&[parent.clone()]);
        let both = walk_roots(&[parent.clone(), child.clone()]);
        assert_eq!(
            once.len(),
            both.len(),
            "Documents under Home must not be indexed twice"
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn terminal_exe_matches_terminal_not_a_fuzzy_component() {
        let mut idx = NameIndex::new();
        idx.insert_meta(
            "JetBrains.ReSharper.Features.WinForms.Designer.External.Core.exe",
            r"C:\Program Files\JetBrains\resharper.exe",
            Some(215_000),
            false,
            Some(1_700_000_000),
        );
        idx.insert_meta(
            "WindowsTerminal.exe",
            r"C:\Program Files\Windows Terminal\WindowsTerminal.exe",
            Some(900_000),
            false,
            Some(1_700_000_100),
        );
        let hits = idx.search("terminal.exe", 8);
        assert!(
            hits.iter().any(|h| h.name == "WindowsTerminal.exe"),
            "{hits:?}"
        );
        assert!(
            hits.iter().all(|h| !h.name.contains("ReSharper")),
            "component exes must not match terminal.exe: {hits:?}"
        );
        assert_eq!(
            hits.iter().find(|h| h.name == "WindowsTerminal.exe").unwrap().modified,
            Some(1_700_000_100)
        );
    }

    fn norm_path(p: &str) -> String {
        p.replace('\\', "/").to_ascii_lowercase()
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "katana-idx-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Project tree the owner searches: css beside the crate, the binary under
    /// `target/release`, a dotfile, a dependency dir, and a file deeper than the
    /// old hard-coded walk cap.
    fn kestrel_tree() -> std::path::PathBuf {
        let root = scratch_dir("kestrel");
        let proj = root.join("kestrel");
        fs::create_dir_all(proj.join("target").join("release")).unwrap();
        fs::create_dir_all(proj.join("node_modules")).unwrap();
        fs::write(root.join("kestrel.css"), b"body{}").unwrap();
        fs::write(root.join("tool.exe"), b"MZ").unwrap();
        fs::write(proj.join("kestrel.css"), b"body{}").unwrap();
        fs::write(proj.join(".envrc"), b"export X=1").unwrap();
        fs::write(proj.join("node_modules").join("left.js"), b"module").unwrap();
        fs::create_dir_all(proj.join("keep-out")).unwrap();
        fs::write(proj.join("keep-out").join("secret.txt"), b"no").unwrap();
        fs::write(
            proj.join("target").join("release").join("kestrel.exe"),
            b"MZ",
        )
        .unwrap();
        let mut deep = root.join("deep");
        for i in 0..16 {
            deep.push(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("deep.txt"), b"deep").unwrap();
        root
    }

    #[test]
    fn extension_query_ranks_exact_filename_when_present() {
        let mut idx = NameIndex::new();
        idx.insert("kestrel", r"C:\Users\rajiv\dev\kestrel");
        idx.insert("kestrel.css", r"C:\Users\rajiv\dev\kestrel\kestrel.css");
        idx.insert(
            "kestrel.exe",
            r"C:\Users\rajiv\dev\kestrel\target\release\kestrel.exe",
        );
        let hits = idx.search("kestrel.exe", 8);
        assert!(
            hits.iter().any(|h| h.name == "kestrel.exe"),
            "a dot in the query must not drop an exact filename: {hits:?}"
        );
        assert_eq!(hits[0].name, "kestrel.exe");
        assert_eq!(hits[0].score, 1.0);
    }

    #[test]
    fn indexed_root_search_finds_every_file_including_exe() {
        let root = kestrel_tree();
        let idx = walk_roots(std::slice::from_ref(&root));
        let exe = idx.search("kestrel.exe", 16);
        assert!(
            exe.iter().any(|h| {
                h.name == "kestrel.exe" && norm_path(&h.path).contains("target/release/kestrel.exe")
            }),
            "target/release/kestrel.exe must be findable under an indexed root: {exe:?}"
        );
        assert!(
            idx.search("tool.exe", 8).iter().any(|h| h.name == "tool.exe"),
            "an .exe next to other files must be indexed"
        );
        assert!(
            idx.search("kestrel.css", 8)
                .iter()
                .any(|h| h.name == "kestrel.css")
        );
        assert!(
            idx.search(".envrc", 8).iter().any(|h| h.name == ".envrc"),
            "dotfiles are indexed unless settings turn them off"
        );
        assert!(
            idx.search("deep.txt", 8).iter().any(|h| h.name == "deep.txt"),
            "a file nested past the old depth cap must still be indexed"
        );
        assert!(
            idx.search("left.js", 8)
                .iter()
                .all(|h| !h.path.contains("node_modules")),
            "node_modules stays out only as a documented default exclusion"
        );
        assert!(
            idx.search("secret.txt", 8).iter().any(|h| h.name == "secret.txt"),
            "a directory absent from the skip list must be indexed"
        );
        assert_eq!(exe[0].name, "kestrel.exe");
        assert_eq!(exe[0].score, 1.0);
        let defaults = default_index_skip_dirs();
        assert!(defaults.iter().any(|s| s == "node_modules"));
        assert!(
            !defaults.iter().any(|s| s.eq_ignore_ascii_case("target")),
            "build output is not a default exclusion"
        );
        assert!(!defaults.iter().any(|s| s.eq_ignore_ascii_case("dist")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exclusions_happen_only_when_options_say_so() {
        let root = kestrel_tree();
        let open = IndexOptions {
            skip_dirs: Vec::new(),
            index_dotfiles: true,
            max_depth: DEFAULT_INDEX_MAX_DEPTH,
        };
        let all = walk_roots_with(std::slice::from_ref(&root), &open);
        assert!(
            all.search("left.js", 8).iter().any(|h| h.name == "left.js"),
            "empty skip list indexes node_modules: {:?}",
            all.search("left.js", 4)
        );
        assert!(all.search("kestrel.exe", 8).iter().any(|h| h.name == "kestrel.exe"));
        assert!(all.search(".envrc", 8).iter().any(|h| h.name == ".envrc"));
        assert!(all.search("secret.txt", 8).iter().any(|h| h.name == "secret.txt"));

        let closed = IndexOptions {
            skip_dirs: vec!["keep-out".into(), "Node_Modules".into()],
            index_dotfiles: false,
            max_depth: DEFAULT_INDEX_MAX_DEPTH,
        };
        let idx = walk_roots_with(std::slice::from_ref(&root), &closed);
        assert!(
            idx.search("secret.txt", 8).is_empty(),
            "keep-out is excluded only because options list it"
        );
        assert!(
            idx.search("left.js", 8)
                .iter()
                .all(|h| !norm_path(&h.path).contains("node_modules")),
            "skip names are case-insensitive"
        );
        assert!(
            idx.search(".envrc", 8).is_empty(),
            "dotfiles stay out when index_dotfiles is false"
        );
        let exe = idx.search("kestrel.exe", 8);
        assert!(
            exe.iter().any(|h| {
                h.name == "kestrel.exe" && norm_path(&h.path).contains("target/release/kestrel.exe")
            }),
            "target is not skipped unless the list says so: {exe:?}"
        );
        assert!(idx.search("tool.exe", 4).iter().any(|h| h.name == "tool.exe"));
        let _ = fs::remove_dir_all(root);
    }
}
