//! Unified search over shipped providers.

use katana_clip::ClipStore;
use katana_core::{
    default_keywords, eval_calc, find_keyword, merge_hits, route, score_hit, Query, Route,
    Hit, HitKind,
};
use katana_index::NameIndex;
use katana_todo::Store as TodoStore;

use crate::apps::{search_apps, AppEntry};
use crate::bookmarks::Bookmark;

pub struct Engine {
    pub keywords: Vec<katana_core::Keyword>,
    pub apps: Vec<AppEntry>,
    pub files: NameIndex,
    pub bookmarks: Vec<Bookmark>,
    pub recents: Vec<Hit>,
}

impl Engine {
    pub fn new(apps: Vec<AppEntry>, files: NameIndex) -> Self {
        Self {
            keywords: default_keywords(),
            apps,
            files,
            bookmarks: Vec::new(),
            recents: Vec::new(),
        }
    }

    pub fn is_keyword(&self, verb: &str) -> bool {
        find_keyword(&self.keywords, verb).is_some()
    }

    pub fn route_str(&self, raw: &str) -> Route {
        let q = Query::parse(raw);
        let known = q
            .verb
            .as_deref()
            .map(|v| self.is_keyword(v))
            .unwrap_or(false);
        route(&q, known)
    }

    pub fn search(
        &self,
        raw: &str,
        todos: Option<&TodoStore>,
        clips: Option<&ClipStore>,
        limit: usize,
    ) -> (Route, Vec<Hit>) {
        let route = self.route_str(raw);
        let hits = self.hits_for(&route, todos, clips, limit);
        (route, hits)
    }

    fn hits_for(
        &self,
        route: &Route,
        todos: Option<&TodoStore>,
        clips: Option<&ClipStore>,
        limit: usize,
    ) -> Vec<Hit> {
        match route {
            Route::Recents => {
                if self.recents.is_empty() {
                    command_hits()
                } else {
                    self.recents.clone()
                }
            }
            Route::Shortcuts { query } | Route::Launch { query } | Route::Apps { query } => {
                launch_hits(self, query, limit)
            }
            Route::Calc { expr } => {
                let title = match eval_calc(expr) {
                    Ok(v) => format!("{v}"),
                    Err(e) => format!("? {e}"),
                };
                vec![Hit::new(format!("calc:{expr}"), title, expr.clone(), 1.0, HitKind::Calc)]
            }
            Route::Shell { cmdline, elevate } => vec![Hit::new(
                format!("sh:{cmdline}"),
                cmdline.clone(),
                if *elevate { "Run elevated" } else { "Run command" },
                1.0,
                HitKind::Shell,
            )],
            Route::Url { url } => {
                vec![Hit::new(format!("url:{url}"), url.clone(), "Open URL", 1.0, HitKind::Keyword)]
            }
            Route::Keyword { name, arg } => {
                if let Some(kw) = find_keyword(&self.keywords, name) {
                    let hint = match arg {
                        Some(a) if !a.is_empty() => a.clone(),
                        _ if kw.url.is_some() => "ctrl+v pastes the search".into(),
                        _ => String::new(),
                    };
                    vec![Hit::new(
                        format!("kw:{name}"),
                        kw.names[0].clone(),
                        hint,
                        1.0,
                        HitKind::Keyword,
                    )]
                } else {
                    Vec::new()
                }
            }
            Route::Files { query } => {
                let q = query.trim();
                if q.is_empty() {
                    return crate::launcher::well_known_folder_hits();
                }
                let mut hits = crate::launcher::matching_folder_hits(q);
                hits.extend(search_apps(&self.apps, q, limit.min(8)));
                for f in self.files.search(q, limit) {
                    hits.push(file_hit(f));
                }
                merge_hits(dedupe_by_path(hits, true), limit)
            }
            Route::Clip { query } => {
                let Some(store) = clips else {
                    return Vec::new();
                };
                let q = query.trim();
                if is_clip_clear_cmd(q) {
                    return vec![clear_clips_hit()];
                }
                let items = if q.is_empty() {
                    store.list(limit).unwrap_or_default()
                } else {
                    store.search(q, limit).unwrap_or_default()
                };
                let mut hits = if q.is_empty() {
                    vec![clear_clips_hit()]
                } else {
                    Vec::new()
                };
                hits.extend(items.into_iter().map(|c| {
                    let image = c.kind == "image";
                    let title = if image {
                        match (c.width, c.height) {
                            (Some(w), Some(h)) => format!("Image  {w}×{h}"),
                            _ => "Image".into(),
                        }
                    } else {
                        c.text.unwrap_or_else(|| format!("[{}]", c.kind))
                    };
                    Hit::new(
                        format!("clip:{}", c.id),
                        title,
                        if c.pinned {
                            if image {
                                "pinned image".into()
                            } else {
                                "pinned".into()
                            }
                        } else if image {
                            "image".into()
                        } else {
                            c.kind
                        },
                        1.0,
                        HitKind::Clip,
                    )
                }));
                hits
            }
            Route::Todo { rest } => {
                let Some(store) = todos else {
                    return Vec::new();
                };
                if rest.trim_start().starts_with('/') {
                    let tasks = match katana_todo::parse_progress_filter(rest) {
                        Some(katana_todo::ProgressFilter::All) => store.list_all().unwrap_or_default(),
                        Some(katana_todo::ProgressFilter::Range { lo, hi }) => {
                            store.list_progress(lo, hi).unwrap_or_default()
                        }
                        Some(katana_todo::ProgressFilter::Rejected) | None => Vec::new(),
                    };
                    return tasks.into_iter().map(todo_hit).collect();
                }
                let (verb, arg) = katana_todo::parse_todo_rest(rest);
                let tasks = match verb {
                    "list" | "l" | "" => store.list_active().unwrap_or_default(),
                    "showall" | "all" | "show-all" => store.list_all().unwrap_or_default(),
                    "add" | "a" => {
                        // preview only — execute_query writes via Store::add
                        store
                            .search(arg)
                            .unwrap_or_default()
                            .into_iter()
                            .chain(std::iter::once(katana_todo::Task {
                                id: 0,
                                title: if arg.is_empty() {
                                    "add …".into()
                                } else {
                                    format!("+ {arg}")
                                },
                                description: String::new(),
                                status: "pending".into(),
                                priority: "medium".into(),
                                progress: 0,
                                tags: "[]".into(),
                                due_date: None,
                                position: 0,
                            }))
                            .collect()
                    }
                    "search" | "find" | "f" => store.search(arg).unwrap_or_default(),
                    _ => {
                        if arg.is_empty() {
                            store.search(verb).unwrap_or_default()
                        } else {
                            store.search(arg).unwrap_or_default()
                        }
                    }
                };
                tasks.into_iter().map(todo_hit).collect()
            }
            Route::KeepAwake => {
                let on = crate::keepawake::is_on();
                vec![Hit::new(
                    "cmd:/awake",
                    if on { "/awake  ● ON" } else { "/awake" },
                    if on {
                        "screen stays awake  ·  ↵ toggle off"
                    } else {
                        "keep screen awake  ·  ↵ toggle on"
                    },
                    1.0,
                    HitKind::Keyword,
                )]
            }
            Route::Settings | Route::EditKeywords | Route::EditTodos => vec![Hit::new(
                "studio",
                "Open editor",
                "Keywords · Todos · Settings",
                1.0,
                HitKind::Keyword,
            )],
            Route::Shot { mode } => {
                let title = match mode {
                    katana_core::ShotMode::Region => "Region  /sr",
                    katana_core::ShotMode::Window => "Window  /sw",
                    katana_core::ShotMode::Screen => "Screen",
                    katana_core::ShotMode::Browser => "Browser page  /sf",
                    katana_core::ShotMode::Last => "Last region",
                    katana_core::ShotMode::Delay { secs } => {
                        return vec![Hit::new(
                            format!("shot:{mode:?}"),
                            format!("Screenshot delay {secs}s"),
                            "Capture",
                            1.0,
                            HitKind::Shot,
                        )];
                    }
                };
                vec![Hit::new(
                    format!("shot:{mode:?}"),
                    title,
                    "clipboard + Pictures\\Katana",
                    1.0,
                    HitKind::Shot,
                )]
            }
            Route::Unified { query } => {
                let mut hits = Vec::new();
                for kw in &self.keywords {
                    let name = &kw.names[0];
                    if katana_core::fuzzy_score(query, name).is_some() {
                        hits.push(Hit::new(
                            format!("kw:{name}"),
                            name.clone(),
                            "shortcut",
                            score_hit(query, name, HitKind::Keyword, 0, 0.0),
                            HitKind::Keyword,
                        ));
                    }
                }
                hits.extend(bookmark_hits(&self.bookmarks, query, limit));
                hits.extend(search_apps(&self.apps, query, limit));
                for f in self.files.search(query, limit) {
                    hits.push(file_hit(f));
                }
                merge_hits(dedupe_by_path(hits, false), limit)
            }
        }
    }
}

/// Mark the alias letter: `/todo` + `t` → `/[t]odo`.
pub fn mark_shortcut(cmd: &str, letter: char) -> String {
    let want = letter.to_ascii_lowercase();
    let mut out = String::with_capacity(cmd.len() + 2);
    let mut marked = false;
    for c in cmd.chars() {
        if !marked && c != '/' && c.to_ascii_lowercase() == want {
            out.push('[');
            out.push(c);
            out.push(']');
            marked = true;
        } else {
            out.push(c);
        }
    }
    if !marked {
        format!("[{letter}]{cmd}")
    } else {
        out
    }
}

fn launch_hits(engine: &Engine, query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim();
    let mut hits = shortcut_hits(engine, query, limit);
    if q.is_empty() {
        hits.extend(engine.apps.iter().take(limit).map(app_hit));
    } else {
        hits.extend(search_apps(&engine.apps, q, limit));
        hits.push(Hit::new(
            format!("sh:{q}"),
            q.to_string(),
            "Run command",
            0.35,
            HitKind::Shell,
        ));
    }
    merge_hits(hits, limit)
}

fn shortcut_hits(engine: &Engine, query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim();
    let mut hits = Vec::new();
    if q.is_empty() || q.eq_ignore_ascii_case("edit") {
        hits.push(Hit::new(
            "cmd:/shortcuts-edit",
            "Edit shortcuts…",
            "your keywords",
            1.1,
            HitKind::Keyword,
        ));
    }
    let ql = q.to_ascii_lowercase();
    for kw in &engine.keywords {
        if q.is_empty()
            || kw
                .names
                .iter()
                .any(|n| n.to_ascii_lowercase().contains(&ql))
        {
            hits.push(Hit::new(
                format!("kw:{}", kw.names[0]),
                kw.names[0].clone(),
                kw.url
                    .as_deref()
                    .or(kw.path.as_deref())
                    .or(kw.command.as_deref())
                    .unwrap_or("shortcut")
                    .replace("$DEF$", " ")
                    .split_whitespace()
                    .next()
                    .unwrap_or("shortcut")
                    .to_string(),
                1.0,
                HitKind::Keyword,
            ));
        }
    }
    hits.extend(bookmark_hits(&engine.bookmarks, q, limit));
    merge_hits(hits, limit)
}

fn bookmark_hits(bookmarks: &[Bookmark], query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim();
    let mut hits = Vec::new();
    if q.is_empty() {
        for b in bookmarks.iter().take(limit) {
            hits.push(bookmark_hit(b, 0.85));
        }
        return hits;
    }
    let mut eng = katana_core::FuzzyEngine::new();
    let pat = katana_core::FuzzyEngine::prepare(q);
    let ql = q.to_ascii_lowercase();
    for b in bookmarks {
        let score = eng
            .score_prepared(&pat, &b.title)
            .or_else(|| {
                b.url
                    .to_ascii_lowercase()
                    .contains(&ql)
                    .then_some(0.5)
            });
        if let Some(score) = score {
            hits.push(bookmark_hit(b, score));
        }
    }
    merge_hits(hits, limit)
}

fn bookmark_hit(b: &Bookmark, score: f32) -> Hit {
    Hit::new(
        format!("bm:{}", b.url),
        b.title.clone(),
        format!("{}  ·  {}", b.source, b.url),
        score,
        HitKind::Keyword,
    )
}

fn command_hits() -> Vec<Hit> {
    [
        ("/todo", Some('t'), "Tasks", HitKind::Todo),
        ("/clip", Some('c'), "Clipboard history", HitKind::Clip),
        ("/shot", Some('s'), "Screenshot", HitKind::Shot),
        ("/awake", None, "Keep screen awake", HitKind::Keyword),
        ("/settings", None, "Settings", HitKind::Keyword),
    ]
    .into_iter()
    .map(|(cmd, letter, subtitle, kind)| {
        let on = cmd == "/awake" && crate::keepawake::is_on();
        let title = if on {
            "/awake  ● ON".to_string()
        } else {
            match letter {
                Some(l) => mark_shortcut(cmd, l),
                None => cmd.to_string(),
            }
        };
        let subtitle = if on {
            "ON  ·  screen stays awake"
        } else {
            subtitle
        };
        Hit {
            id: format!("cmd:{cmd}"),
            title,
            subtitle: subtitle.into(),
            score: 1.0,
            kind,
            size: None,
            type_name: None,
            is_dir: false,
            modified: None,
        }
    })
    .collect()
}

pub fn is_home_query(q: &str) -> bool {
    q.trim().is_empty()
}

pub fn is_todo_blade(q: &str) -> bool {
    matches!(katana_core::slash_verb(q), Some("t" | "todo"))
}

pub fn is_clip_blade(q: &str) -> bool {
    matches!(katana_core::slash_verb(q), Some("c" | "clip"))
}

pub fn is_file_blade(q: &str) -> bool {
    matches!(katana_core::slash_verb(q), Some("f" | "file" | "files"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSort {
    Score,
    Name,
    Modified,
    Type,
    Size,
}

/// Column x hits in the file details header (must match overlay paint).
pub fn file_column_at(x: i32) -> FileSort {
    if x < 330 {
        FileSort::Name
    } else if x < 490 {
        FileSort::Modified
    } else if x < 640 {
        FileSort::Type
    } else {
        FileSort::Size
    }
}

pub fn sort_file_hits(hits: &mut [Hit], sort: FileSort, desc: bool) {
    hits.sort_by(|a, b| {
        let ord = match sort {
            FileSort::Score => b
                .score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.title.to_ascii_lowercase().cmp(&b.title.to_ascii_lowercase())),
            FileSort::Name => a
                .title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase()),
            FileSort::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0)),
            FileSort::Type => a
                .type_name
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .cmp(&b.type_name.as_deref().unwrap_or("").to_ascii_lowercase()),
            FileSort::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
        };
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

pub const FILE_PAGE: usize = 8;
pub const FILE_SEARCH_LIMIT: usize = 64;

pub fn clamp_caret(len: usize, caret: usize) -> usize {
    caret.min(len)
}

pub fn insert_str_at(s: &str, caret: usize, extra: &str) -> (String, usize) {
    let n = s.chars().count();
    let caret = caret.min(n);
    let mut out = String::with_capacity(s.len() + extra.len());
    for (i, c) in s.chars().enumerate() {
        if i == caret {
            out.push_str(extra);
        }
        out.push(c);
    }
    if caret == n {
        out.push_str(extra);
    }
    (out, caret + extra.chars().count())
}

pub fn insert_at(s: &str, caret: usize, ch: char) -> (String, usize) {
    let n = s.chars().count();
    let caret = caret.min(n);
    let mut out = String::with_capacity(s.len() + ch.len_utf8());
    for (i, c) in s.chars().enumerate() {
        if i == caret {
            out.push(ch);
        }
        out.push(c);
    }
    if caret == n {
        out.push(ch);
    }
    (out, caret + 1)
}

pub fn backspace_at(s: &str, caret: usize) -> (String, usize) {
    if caret == 0 {
        return (s.to_string(), 0);
    }
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if i + 1 != caret {
            out.push(c);
        }
    }
    (out, caret - 1)
}

pub fn delete_at(s: &str, caret: usize) -> (String, usize) {
    let n = s.chars().count();
    let caret = caret.min(n);
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if i != caret {
            out.push(c);
        }
    }
    let n = out.chars().count();
    (out, caret.min(n))
}

pub fn move_caret(len: usize, caret: usize, delta: i32) -> usize {
    let caret = caret.min(len);
    if delta < 0 {
        caret.saturating_sub((-delta) as usize)
    } else {
        (caret + delta as usize).min(len)
    }
}

/// Keep `selected` inside the visible page of `vis` rows.
pub fn file_scroll_keep_visible(len: usize, vis: usize, selected: usize, scroll: usize) -> usize {
    if len <= vis {
        return 0;
    }
    let vis = vis.max(1);
    let max_scroll = len - vis;
    let sel = selected.min(len.saturating_sub(1));
    let mut s = scroll.min(max_scroll);
    if sel < s {
        s = sel;
    } else if sel >= s + vis {
        s = sel + 1 - vis;
    }
    s.min(max_scroll)
}

/// Thumb offset and height inside a track of `track_h` pixels. `None` if everything fits.
pub fn scrollbar_thumb(len: usize, vis: usize, scroll: usize, track_h: i32) -> Option<(i32, i32)> {
    if len <= vis || track_h < 12 {
        return None;
    }
    let thumb_h = ((track_h as usize * vis) / len).clamp(12, track_h as usize);
    let max_scroll = len - vis;
    let travel = (track_h as usize).saturating_sub(thumb_h);
    let y = if max_scroll == 0 {
        0
    } else {
        travel * scroll.min(max_scroll) / max_scroll
    };
    Some((y as i32, thumb_h as i32))
}

/// Horizontal shift so the caret stays inside a prompt of `avail` pixels.
pub fn prompt_scroll_px(caret_px: i32, avail: i32) -> i32 {
    let avail = avail.max(8);
    (caret_px + 6 - avail).max(0)
}

pub fn scroll_from_track_click(len: usize, vis: usize, track_h: i32, click_y: i32) -> usize {
    if len <= vis || track_h <= 0 {
        return 0;
    }
    let max_scroll = len - vis;
    let y = click_y.clamp(0, track_h) as usize;
    max_scroll * y / track_h as usize
}

pub fn clip_hit_is_image(h: &Hit) -> bool {
    h.kind == HitKind::Clip
        && h.id != "clip:clear"
        && (h.subtitle.eq_ignore_ascii_case("image")
            || h.subtitle.to_ascii_lowercase().contains("image"))
}

pub fn is_blade_query(q: &str) -> bool {
    q.trim_start().starts_with('/')
}

/// One Esc step: `Some("/clip")` from `/clip foo`, `Some("")` from a blade root, `None` to hide.
pub fn parent_query(raw: &str) -> Option<String> {
    let q = raw.trim();
    if q.is_empty() {
        return None;
    }
    if let Some(verb) = katana_core::slash_verb(q) {
        let rest = q
            .split_once(char::is_whitespace)
            .map(|(_, r)| r.trim())
            .unwrap_or("");
        if rest.is_empty() {
            return Some(String::new());
        }
        return Some(format!("/{verb}"));
    }
    None
}

pub fn todo_list_mode(q: &str) -> bool {
    if !is_todo_blade(q) {
        return false;
    }
    let rest = q.trim().split_once(char::is_whitespace).map(|(_, r)| r.trim()).unwrap_or("");
    rest.is_empty()
        || rest.starts_with('/')
        || matches!(rest, "showall" | "all" | "show-all")
}

/// Blade footer, or `None` on home (no leftover tool hints).
pub fn blade_footer(q: &str, composing: bool) -> Option<&'static str> {
    if is_home_query(q) {
        return None;
    }
    if is_todo_blade(q) {
        return Some(if composing {
            "↵ save    esc cancel"
        } else {
            "+ add    ↵ edit    % progress    / 0..100    alt+v done    esc back"
        });
    }
    if is_clip_blade(q) {
        return Some("↵ paste    del remove    esc back");
    }
    match katana_core::slash_verb(q) {
        Some("shot" | "ss" | "s" | "sr" | "sw" | "sf") => {
            Some("↵ capture    /sr region    /sw window    /sf browser    esc back")
        }
        Some("f" | "file" | "files") => {
            Some("↵ open    ctrl+c copy path    click headers to sort    esc back")
        }
        Some(
            "apps" | "app" | "a" | "cmd" | "go" | "launch" | "bm" | "b" | "bookmark" | "bookmarks"
                | "shortcuts" | "sc" | "k" | "kw" | "keywords",
        ) => Some("↵ open / run    esc back"),
        Some("awake" | "keepawake" | "ka") => Some("↵ toggle keep-awake    esc back"),
        Some(_) => Some("esc back"),
        None => Some("↵ open    esc hide    ↑↓ select"),
    }
}

fn todo_hit(t: katana_todo::Task) -> Hit {
    Hit::new(
        format!("todo:{}", t.id),
        t.title,
        format!(
            "#{} · {} · {}",
            t.id,
            katana_todo::format_progress(t.progress),
            t.status.replace('_', " ")
        ),
        1.0,
        HitKind::Todo,
    )
}

fn parent_label(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

fn file_hit(f: katana_index::FileHit) -> Hit {
    let mut h = Hit::new(
        format!("file:{}", f.path),
        f.name,
        parent_label(&f.path),
        f.score,
        HitKind::File,
    );
    h.size = f.size;
    h.type_name = Some(f.type_name);
    h.is_dir = f.is_dir;
    h.modified = f.modified;
    h
}

/// Same path can arrive as a program and as a file. Keep one.
/// `prefer_file` keeps the file row so `/f` can show date and size.
fn dedupe_by_path(hits: Vec<Hit>, prefer_file: bool) -> Vec<Hit> {
    let mut order = Vec::new();
    let mut map: std::collections::HashMap<String, Hit> = std::collections::HashMap::new();
    for h in hits {
        let key = launch_path(&h)
            .map(|p| p.to_ascii_lowercase())
            .unwrap_or_else(|| h.id.clone());
        match map.get(&key) {
            None => {
                order.push(key.clone());
                map.insert(key, h);
            }
            Some(prev) => {
                let replace = if h.kind == prev.kind {
                    h.score > prev.score
                } else if prefer_file {
                    h.kind == HitKind::File
                } else {
                    h.kind == HitKind::App
                };
                if replace {
                    map.insert(key, h);
                }
            }
        }
    }
    order.into_iter().filter_map(|k| map.remove(&k)).collect()
}

fn app_hit(a: &AppEntry) -> Hit {
    Hit::new(
        format!("app:{}", a.path.display()),
        a.name.clone(),
        "Application",
        1.0,
        HitKind::App,
    )
}

pub fn is_clip_clear_cmd(q: &str) -> bool {
    matches!(
        q.trim().to_ascii_lowercase().as_str(),
        "clear" | "clearall" | "wipe" | "empty"
    )
}

fn clear_clips_hit() -> Hit {
    Hit::new(
        "clip:clear",
        "Clear all clipboard history",
        "removes every saved clip",
        1.0,
        HitKind::Clip,
    )
}

/// Date modified for the file table. Local time on Windows, UTC elsewhere.
pub fn format_modified(unix_secs: u64) -> String {
    #[cfg(windows)]
    {
        if let Some(s) = format_modified_local(unix_secs) {
            return s;
        }
    }
    katana_index::format_mtime(unix_secs)
}

#[cfg(windows)]
fn format_modified_local(unix_secs: u64) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ticks = (unix_secs as u64)
        .checked_add(11_644_473_600)?
        .checked_mul(10_000_000)?;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
    ))
}

pub fn launch_path(h: &Hit) -> Option<String> {
    for prefix in ["file:", "app:"] {
        if let Some(p) = h.id.strip_prefix(prefix) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use katana_index::NameIndex;

    fn eng() -> Engine {
        let mut files = NameIndex::new();
        files.insert("invoice.pdf", r"C:\docs\invoice.pdf");
        Engine::new(Vec::new(), files)
    }

    #[test]
    fn router_verbs_g_todo_clip_shot_f_shell() {
        let e = eng();
        assert!(matches!(e.route_str("g rust"), Route::Keyword { name, .. } if name == "g"));
        assert!(matches!(e.route_str("/todo add x"), Route::Todo { .. }));
        assert!(matches!(e.route_str("/t a x"), Route::Todo { .. }));
        assert!(matches!(e.route_str("/clip foo"), Route::Clip { .. }));
        assert!(matches!(e.route_str("/shot last"), Route::Shot { .. }));
        assert!(matches!(e.route_str("/ss"), Route::Shot { .. }));
        assert!(matches!(e.route_str("/sr"), Route::Shot { mode: katana_core::ShotMode::Region }));
        assert!(matches!(e.route_str("/sw"), Route::Shot { mode: katana_core::ShotMode::Window }));
        assert!(matches!(e.route_str("/sf"), Route::Shot { mode: katana_core::ShotMode::Browser }));
        assert!(matches!(e.route_str("/f invoice.pdf"), Route::Files { .. }));
        assert!(matches!(e.route_str("/apps"), Route::Launch { .. }));
        assert!(matches!(e.route_str("/cmd dir"), Route::Launch { .. }));
        assert!(matches!(e.route_str("/k foo"), Route::Launch { .. }));
        assert!(matches!(e.route_str("todo add x"), Route::Unified { .. }));
        assert!(matches!(e.route_str("> cargo test"), Route::Shell { elevate: false, .. }));
        assert!(matches!(e.route_str(">! diskpart"), Route::Shell { elevate: true, .. }));
        assert!(matches!(e.route_str("=2^9"), Route::Calc { .. }));
    }

    #[test]
    fn file_empty_query_lists_user_folders() {
        let e = eng();
        let (r, hits) = e.search("/f", None, None, 16);
        assert!(matches!(r, Route::Files { .. }));
        if std::env::var("USERPROFILE").is_ok() {
            let names: Vec<_> = hits.iter().map(|h| h.title.as_str()).collect();
            assert!(
                names.iter().any(|n| ["Documents", "Downloads", "Desktop", "Home"].contains(n)),
                "{names:?}"
            );
            assert!(hits.iter().all(|h| h.is_dir));
            assert!(
                hits.iter().any(|h| h.modified.is_some()),
                "folder rows need a modified time"
            );
        }
        let (_, dl) = e.search("/f download", None, None, 16);
        assert!(
            dl.iter().any(|h| h.title.eq_ignore_ascii_case("Downloads") && h.is_dir)
                || dl.is_empty() && well_known_missing("Downloads"),
            "{:?}",
            dl.iter().map(|h| &h.title).collect::<Vec<_>>()
        );
    }

    fn well_known_missing(name: &str) -> bool {
        crate::launcher::well_known_user_folders()
            .iter()
            .all(|(n, _)| n != name)
    }

    #[test]
    fn file_route_uses_index() {
        let mut e = eng();
        let (r, hits) = e.search("/f invoice", None, None, 8);
        assert!(matches!(r, Route::Files { .. }));
        assert!(
            hits.iter().any(|h| h.title == "invoice.pdf"),
            "{hits:?}"
        );
        let inv = hits.iter().find(|h| h.title == "invoice.pdf").unwrap();
        assert_eq!(launch_path(inv).as_deref(), Some(r"C:\docs\invoice.pdf"));
        assert_ne!(inv.subtitle, r"C:\docs\invoice.pdf");
        e.files
            .insert_meta("dated.pdf", r"C:\docs\dated.pdf", Some(12), false, Some(1_700_000_000));
        let (_, dated) = e.search("/f dated.pdf", None, None, 8);
        let dated = dated.iter().find(|h| h.title == "dated.pdf").unwrap();
        assert_eq!(dated.modified, Some(1_700_000_000));
        e.files.insert_full("report.pdf", r"C:\a\report.pdf", Some(4096), false);
        let (_, wild) = e.search("/f *.pdf", None, None, 8);
        assert!(
            wild.iter().any(|h| h.title.ends_with(".pdf") && h.type_name.as_deref() == Some("PDF File")),
            "{wild:?}"
        );
    }

    #[test]
    fn calc_hit_uses_shipped_eval() {
        let e = eng();
        let (_, hits) = e.search("=2^9", None, None, 1);
        assert_eq!(hits[0].title, "512");
    }

    #[test]
    fn empty_query_shows_command_palette() {
        let e = eng();
        let (r, hits) = e.search("", None, None, 16);
        assert!(matches!(r, Route::Recents));
        assert!(hits.iter().any(|h| h.title == "/[t]odo"), "{:?}", hits.iter().map(|h| &h.title).collect::<Vec<_>>());
        assert!(hits.iter().any(|h| h.title == "/[s]hot"));
        assert!(
            !hits.iter().any(|h| h.id == "cmd:/k" || h.id == "cmd:/f"),
            "home hides /k and /f; type them"
        );
        assert!(!hits.iter().any(|h| h.id == "cmd:/apps" || h.id == "cmd:/cmd"));
        assert!(!hits.iter().any(|h| h.id == "cmd:/bm" || h.id == "cmd:/keywords"));
        assert!(hits.iter().any(|h| h.id == "cmd:/todo"));
        assert!(
            !hits.iter().any(|h| h.id == "cmd:g" || h.title == "g"),
            "g is a keyword, not a home-page row"
        );
        assert!(blade_footer("", false).is_none());
        assert!(blade_footer("/f", false).unwrap().contains("copy path"));
        assert!(blade_footer("/todo", false).unwrap().contains("alt+v"));
        assert!(blade_footer("/clip", false).unwrap().contains("paste"));
        assert_eq!(parent_query(""), None);
        assert_eq!(parent_query("/clip").as_deref(), Some(""));
        assert_eq!(parent_query("/clip secret").as_deref(), Some("/clip"));
        assert_eq!(parent_query("/todo showall").as_deref(), Some("/todo"));
        assert_eq!(parent_query("/f invoice.pdf").as_deref(), Some("/f"));
        assert_eq!(parent_query("/sw").as_deref(), Some(""));
        assert_eq!(parent_query("notepad"), None);
        assert!(is_blade_query("/clip") && !is_blade_query("clip"));
        assert_eq!(file_column_at(40), FileSort::Name);
        assert_eq!(file_column_at(340), FileSort::Modified);
        assert_eq!(file_column_at(500), FileSort::Type);
        assert_eq!(file_column_at(650), FileSort::Size);
        let mut hits = vec![
            {
                let mut h = Hit::new("file:a", "zeta.pdf", "", 0.2, HitKind::File);
                h.size = Some(100);
                h.type_name = Some("PDF File".into());
                h.modified = Some(50);
                h
            },
            {
                let mut h = Hit::new("file:b", "alpha.txt", "", 0.9, HitKind::File);
                h.size = Some(9);
                h.type_name = Some("Text Document".into());
                h.modified = Some(80);
                h
            },
        ];
        sort_file_hits(&mut hits, FileSort::Name, false);
        assert_eq!(hits[0].title, "alpha.txt");
        sort_file_hits(&mut hits, FileSort::Size, true);
        assert_eq!(hits[0].title, "zeta.pdf");
        sort_file_hits(&mut hits, FileSort::Modified, true);
        assert_eq!(hits[0].title, "alpha.txt");
        let (s, c) = insert_at("/f ", 3, 'a');
        assert_eq!((s.as_str(), c), ("/f a", 4));
        let (s, c) = insert_at("/f hello", 4, 'X');
        assert_eq!((s.as_str(), c), ("/f hXello", 5));
        let (s, c) = backspace_at("/f ab", 4);
        assert_eq!((s.as_str(), c), ("/f b", 3));
        let (s, c) = delete_at("/f ab", 3);
        assert_eq!((s.as_str(), c), ("/f b", 3));
        assert_eq!(move_caret(5, 5, -1), 4);
        assert_eq!(move_caret(5, 0, -1), 0);
        assert_eq!(move_caret(5, 2, 9), 5);
        assert_eq!(file_scroll_keep_visible(20, 8, 0, 0), 0);
        assert_eq!(file_scroll_keep_visible(20, 8, 10, 0), 3);
        assert_eq!(file_scroll_keep_visible(20, 8, 2, 5), 2);
        assert_eq!(file_scroll_keep_visible(5, 8, 4, 3), 0);
        let (y, h) = scrollbar_thumb(20, 8, 0, 100).unwrap();
        assert_eq!(y, 0);
        assert!(h >= 12 && h < 100);
        assert_eq!(scroll_from_track_click(20, 8, 100, 0), 0);
        assert_eq!(scroll_from_track_click(20, 8, 100, 100), 12);
        assert_eq!(prompt_scroll_px(40, 200), 0);
        assert_eq!(prompt_scroll_px(250, 200), 56);
        let mut s = "/f ".to_string();
        let mut c = 3usize;
        for ch in ['a', 'b', 'c'] {
            let n = insert_at(&s, c, ch);
            s = n.0;
            c = n.1;
        }
        assert_eq!(s, "/f abc");
        assert_eq!(c, 6);
    }

    #[test]
    fn mark_shortcut_brackets_the_letter() {
        assert_eq!(mark_shortcut("/todo", 't'), "/[t]odo");
        assert_eq!(mark_shortcut("/keywords", 'k'), "/[k]eywords");
        assert_eq!(mark_shortcut("/clip", 'c'), "/[c]lip");
        assert_eq!(mark_shortcut("/f", 'f'), "/[f]");
        assert!(clip_hit_is_image(&Hit::new("clip:1", "Image  8×8", "image", 1.0, HitKind::Clip)));
        assert!(!clip_hit_is_image(&Hit::new(
            "clip:clear",
            "Clear all clipboard history",
            "removes every saved clip",
            1.0,
            HitKind::Clip
        )));
    }

    #[test]
    fn short_aliases_route() {
        let e = eng();
        assert!(matches!(e.route_str("/c"), Route::Clip { .. }));
        assert!(matches!(e.route_str("/s"), Route::Shot { .. }));
        assert!(matches!(e.route_str("/b"), Route::Launch { .. }));
        assert!(matches!(e.route_str("/k"), Route::Launch { .. }));
        assert!(matches!(e.route_str("/a"), Route::Launch { .. }));
        assert!(matches!(e.route_str("/cmd"), Route::Launch { .. }));
        assert!(is_todo_blade("/t showall") && todo_list_mode("/todo showall"));
        assert!(todo_list_mode("/todo /0..10") && todo_list_mode("/todo /"));
        assert!(!todo_list_mode("/todo add milk"));
        assert!(blade_footer("/todo", false).unwrap().contains("/ 0..100"));
    }

    #[test]
    fn todo_slash_filters_progress_including_completed() {
        let path = std::env::temp_dir().join(format!(
            "katana-search-todo-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = katana_todo::Store::open(&path).unwrap();
        let low = store
            .add("low", "", katana_todo::Priority::Medium, katana_todo::Status::Pending, 0, "[]", None)
            .unwrap();
        let done = store
            .add("done-title-0..10", "", katana_todo::Priority::Medium, katana_todo::Status::Pending, 0, "[]", None)
            .unwrap();
        store.set_progress(low.id, 4).unwrap();
        store.set_progress(done.id, 100).unwrap();
        let e = eng();
        let (_, active) = e.search("/todo", Some(&store), None, 16);
        assert!(active.iter().any(|h| h.title == "low"));
        assert!(!active.iter().any(|h| h.id == format!("todo:{}", done.id)));
        let (_, all) = e.search("/todo /", Some(&store), None, 16);
        assert!(all.iter().any(|h| h.title == "done-title-0..10"));
        let (_, band) = e.search("/todo /0..10", Some(&store), None, 16);
        assert_eq!(band.len(), 1);
        assert_eq!(band[0].title, "low");
        let (_, exact) = e.search("/todo /100", Some(&store), None, 16);
        assert_eq!(exact.len(), 1);
        assert!(exact[0].title.starts_with("done-title"));
        let (_, open) = e.search("/todo /10..", Some(&store), None, 16);
        assert_eq!(open.len(), 1);
        let (_, junk) = e.search("/todo /;drop", Some(&store), None, 16);
        assert!(junk.is_empty(), "junk filter matches nothing, not titles");
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn bookmarks_lists_url_keywords() {
        let e = eng();
        let (r, hits) = e.search("/bm", None, None, 16);
        assert!(matches!(r, Route::Launch { .. }));
        assert!(
            hits.iter().any(|h| h.title == "g"),
            "bookmarks should include google shortcut: {hits:?}"
        );
    }

    #[test]
    fn launch_combines_apps_shortcuts_and_run_command() {
        let mut e = eng();
        e.apps.push(crate::apps::AppEntry {
            name: "Notepad".into(),
            path: r"C:\Windows\notepad.exe".into(),
        });
        let (r, hits) = e.search("/k notepad", None, None, 16);
        assert!(matches!(r, Route::Launch { .. }));
        assert!(
            hits.iter().any(|h| h.kind == HitKind::App && h.title == "Notepad"),
            "apps: {hits:?}"
        );
        assert!(
            hits.iter().any(|h| h.kind == HitKind::Shell && h.title == "notepad"),
            "run command: {hits:?}"
        );
        let (_, empty) = e.search("/a", None, None, 16);
        assert!(empty.iter().any(|h| h.kind == HitKind::App));
        assert!(empty.iter().any(|h| h.id == "cmd:/shortcuts-edit" || h.kind == HitKind::Keyword));
    }
}
