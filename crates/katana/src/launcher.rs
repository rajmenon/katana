//! Resident palette: type → route → execute. Owns todo + clip + index.

use katana_clip::ClipStore;
use katana_core::{
    find_keyword, parse_shot_rest, resolve_keyword, Hit, ResolvedAction, Route, ShotMode,
};
use katana_index::NameIndex;
use katana_shot::{dib_from_bgra, png_from_bgra, PhysRect};
use katana_todo::{parse_todo_rest, Priority, Status, Store as TodoStore};

use crate::apps::{default_app_dirs, scan_apps};
use crate::exec::perform;
use crate::search::Engine;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Outcome {
    Ran(String),
    Copied(String),
    Todo(String),
    Shot { width: u32, height: u32, png_len: usize, dib_len: usize },
    Listed(Vec<Hit>),
    NeedCapture(ShotMode),
    OpenStudio(&'static str),
}

#[derive(Debug, Clone)]
pub enum TodoInput {
    Add { buf: String },
    Edit { id: i64, buf: String },
    Progress { id: i64, buf: String },
}

pub struct Launcher {
    pub engine: Engine,
    pub todos: TodoStore,
    pub clips: ClipStore,
    pub last_region: Option<PhysRect>,
    pub query: String,
    pub hits: Vec<Hit>,
    pub selected: usize,
    pub dry_run: bool,
    pub clip_history_limit: usize,
    pub todo_input: Option<TodoInput>,
    clip_preview: Option<(i64, u32, u32, Vec<u8>)>,
}

impl Launcher {
    pub fn open(
        todo_db: &std::path::Path,
        clip_db: &std::path::Path,
        files: NameIndex,
        dry_run: bool,
    ) -> Result<Self, String> {
        let engine = Engine::new(Vec::new(), files);
        let todos = TodoStore::open(todo_db).map_err(|e| e.to_string())?;
        let clips = ClipStore::open(clip_db).map_err(|e| e.to_string())?;
        Ok(Self {
            engine,
            todos,
            clips,
            last_region: None,
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            dry_run,
            clip_history_limit: 100,
            todo_input: None,
            clip_preview: None,
        })
    }

    pub fn open_default(dry_run: bool) -> Result<Self, String> {
        let (todos, todo_path) = TodoStore::open_default().map_err(|e| e.to_string())?;
        let clip_path = todo_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("clipboard.db");
        // Fast path: tray first. File/app index fills on a background thread.
        let clips = ClipStore::open(&clip_path).map_err(|e| e.to_string())?;
        let mut engine = Engine::new(Vec::new(), NameIndex::new());
        engine.keywords = crate::persist::ensure_keyword_file();
        let settings = crate::persist::Settings::load(&crate::persist::settings_path());
        let clip_history_limit =
            crate::persist::clamp_clip_history_limit(settings.clip_history_limit);
        let _ = clips.trim_to(clip_history_limit);
        Ok(Self {
            engine,
            todos,
            clips,
            last_region: None,
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            dry_run,
            clip_history_limit,
            todo_input: None,
            clip_preview: None,
        })
    }

    pub fn set_query(&mut self, raw: &str) -> Vec<Hit> {
        self.todo_input = None;
        self.query = raw.to_string();
        let (_, hits) = self
            .engine
            .search(raw, Some(&self.todos), Some(&self.clips), 24);
        self.hits = hits;
        self.selected = 0;
        self.hits.clone()
    }

    pub fn execute_query(&mut self, raw: &str) -> Result<Outcome, String> {
        self.set_query(raw);
        self.execute_current()
    }

    pub fn execute_current(&mut self) -> Result<Outcome, String> {
        let route = self.engine.route_str(&self.query);
        match route {
            Route::Calc { expr } => {
                let v = katana_core::eval_calc(&expr).map_err(|e| e.to_string())?;
                let s = format!("{v}");
                if !self.dry_run {
                    let _ = crate::capture::set_clipboard_text(&s);
                }
                Ok(Outcome::Copied(s))
            }
            Route::Shell { cmdline, elevate } => {
                self.run_action(&ResolvedAction::Shell { cmdline, admin: elevate })
            }
            Route::Url { url } => self.run_action(&ResolvedAction::OpenUrl(url)),
            Route::Keyword { name, arg } => {
                let kw = find_keyword(&self.engine.keywords, &name)
                    .ok_or_else(|| format!("unknown keyword {name}"))?;
                let clip = self
                    .clips
                    .list(1)
                    .ok()
                    .and_then(|v| v.into_iter().next())
                    .and_then(|c| c.text)
                    .unwrap_or_default();
                let acts = resolve_keyword(kw, arg.as_deref(), &clip);
                let mut msgs = Vec::new();
                for a in &acts {
                    match self.run_action(a)? {
                        Outcome::Ran(s) => msgs.push(s),
                        other => return Ok(other),
                    }
                }
                Ok(Outcome::Ran(msgs.join(" || ")))
            }
            Route::Todo { rest } => self.execute_todo(&rest),
            Route::Shot { mode } => Ok(Outcome::NeedCapture(mode)),
            Route::Settings => Ok(Outcome::OpenStudio("settings")),
            Route::EditKeywords => Ok(Outcome::OpenStudio("shortcuts")),
            Route::EditTodos => Ok(Outcome::OpenStudio("todos")),
            Route::Clip { query } if crate::search::is_clip_clear_cmd(&query) => {
                self.clear_clips()
            }
            Route::Shortcuts { .. }
            | Route::Clip { .. }
            | Route::Files { .. }
            | Route::Apps { .. }
            | Route::Unified { .. }
            | Route::Recents => self.execute_selected_hit(),
        }
    }

    fn execute_selected_hit(&mut self) -> Result<Outcome, String> {
        let hit = self
            .hits
            .get(self.selected)
            .cloned()
            .ok_or_else(|| "nothing selected".to_string())?;
        if let Some(cmd) = hit.id.strip_prefix("cmd:") {
            if hit.kind == katana_core::HitKind::Shot {
                return Ok(Outcome::NeedCapture(ShotMode::Region));
            }
            if cmd == "/shortcuts-edit" {
                return Ok(Outcome::OpenStudio("shortcuts"));
            }
            if cmd == "/settings" {
                return Ok(Outcome::OpenStudio("settings"));
            }
            let q = if cmd == "/cmd" {
                "/cmd ".to_string()
            } else {
                cmd.to_string()
            };
            self.set_query(&q);
            return Ok(Outcome::Listed(self.hits.clone()));
        }
        if let Some(url) = hit.id.strip_prefix("bm:") {
            return self.run_action(&ResolvedAction::OpenUrl(url.to_string()));
        }
        match hit.kind {
            katana_core::HitKind::Keyword => self.execute_query(&hit.title),
            katana_core::HitKind::App | katana_core::HitKind::File => {
                let path = crate::search::launch_path(&hit)
                    .or_else(|| {
                        if hit.subtitle.contains('\\') {
                            Some(hit.subtitle.clone())
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| "no path for that item".to_string())?;
                self.run_action(&ResolvedAction::Launch {
                    path,
                    args: Vec::new(),
                    admin: false,
                })
            }
            katana_core::HitKind::Clip => self.execute_clip(&hit),
            katana_core::HitKind::Todo => {
                self.begin_todo_edit();
                Ok(Outcome::Listed(self.hits.clone()))
            }
            katana_core::HitKind::Calc => Ok(Outcome::Copied(hit.title)),
            katana_core::HitKind::Shell => self.run_action(&ResolvedAction::Shell {
                cmdline: hit.title,
                admin: false,
            }),
            katana_core::HitKind::Shot => Ok(Outcome::NeedCapture(ShotMode::Region)),
        }
    }

    fn execute_todo(&mut self, rest: &str) -> Result<Outcome, String> {
        let (verb, arg) = parse_todo_rest(rest);
        match verb {
            "add" | "a" => {
                if arg.is_empty() {
                    return Err("todo add TITLE".into());
                }
                let t = self
                    .todos
                    .add(arg, "", Priority::Medium, Status::Pending, 0, "[]", None)
                    .map_err(|e| e.to_string())?;
                self.set_query("/todo");
                Ok(Outcome::Todo(format!("Added task #{}: {}", t.id, t.title)))
            }
            "list" | "l" | "" => {
                self.set_query("/todo");
                Ok(Outcome::Listed(self.hits.clone()))
            }
            "showall" | "all" | "show-all" => {
                self.set_query("/todo showall");
                Ok(Outcome::Listed(self.hits.clone()))
            }
            "done" | "d" => {
                let id: i64 = arg.parse().map_err(|_| "todo done ID")?;
                let t = self
                    .todos
                    .set_status(id, Status::Completed, Some(100))
                    .map_err(|e| e.to_string())?;
                Ok(Outcome::Todo(format!("Completed #{}: {}", t.id, t.title)))
            }
            "undone" | "u" => {
                let id: i64 = arg.parse().map_err(|_| "todo undone ID")?;
                let t = self
                    .todos
                    .set_status(id, Status::Pending, Some(0))
                    .map_err(|e| e.to_string())?;
                Ok(Outcome::Todo(format!("Reopened #{}: {}", t.id, t.title)))
            }
            "start" | "b" | "begin" => {
                let id: i64 = arg.parse().map_err(|_| "todo start ID")?;
                let t = self
                    .todos
                    .set_status(id, Status::InProgress, Some(1))
                    .map_err(|e| e.to_string())?;
                Ok(Outcome::Todo(format!("Started #{}: {}", t.id, t.title)))
            }
            "progress" | "p" | "%" => {
                let mut parts = arg.split_whitespace();
                let id: i64 = parts
                    .next()
                    .ok_or("todo progress ID N")?
                    .parse()
                    .map_err(|_| "todo progress ID N")?;
                let n = katana_todo::parse_percent(parts.next().unwrap_or(""))
                    .ok_or("todo progress ID N")?;
                let t = self
                    .todos
                    .set_progress(id, n)
                    .map_err(|e| e.to_string())?;
                self.set_query("/todo");
                Ok(Outcome::Todo(format!(
                    "#{} → {} ({})",
                    t.id,
                    katana_todo::format_progress(t.progress),
                    t.title
                )))
            }
            "rm" | "r" => {
                let id: i64 = arg.parse().map_err(|_| "todo rm ID")?;
                self.todos.delete(id, true).map_err(|e| e.to_string())?;
                Ok(Outcome::Todo(format!("Deleted #{id}")))
            }
            "search" | "find" | "f" => {
                self.set_query(&format!("/todo {arg}"));
                Ok(Outcome::Listed(self.hits.clone()))
            }
            other => {
                self.set_query(&format!("/todo {other} {arg}"));
                Ok(Outcome::Listed(self.hits.clone()))
            }
        }
    }

    fn execute_clip(&mut self, hit: &katana_core::Hit) -> Result<Outcome, String> {
        if hit.id == "clip:clear" {
            return self.clear_clips();
        }
        if self.dry_run {
            return Ok(Outcome::Copied(hit.title.clone()));
        }
        let id = hit
            .id
            .strip_prefix("clip:")
            .and_then(|s| s.parse::<i64>().ok());
        if hit.subtitle.contains("image") {
            if let Some(id) = id {
                if let Ok(Some(png)) = self.clips.get_image(id) {
                    if let Ok((w, h, bgra)) = katana_shot::png_to_bgra(&png) {
                        if let Ok(dib) = katana_shot::dib_from_bgra(w, h, &bgra) {
                            let _ = crate::capture::set_clipboard_png_dib(&png, &dib);
                        }
                    }
                    return Ok(Outcome::Copied("image".into()));
                }
            }
        }
        let _ = crate::capture::set_clipboard_text(&hit.title);
        Ok(Outcome::Copied(hit.title.clone()))
    }

    pub fn clear_clips(&mut self) -> Result<Outcome, String> {
        let _n = self.clips.clear_all().map_err(|e| e.to_string())?;
        self.clip_preview = None;
        self.set_query("/clip");
        Ok(Outcome::Listed(self.hits.clone()))
    }

    pub fn delete_selected_clip(&mut self) -> Result<Outcome, String> {
        let hit = self
            .hits
            .get(self.selected)
            .cloned()
            .ok_or_else(|| "nothing selected".to_string())?;
        if hit.id == "clip:clear" {
            return self.clear_clips();
        }
        let Some(id) = hit.id.strip_prefix("clip:").and_then(|s| s.parse::<i64>().ok()) else {
            return Err("not a clip item".into());
        };
        let ok = self.clips.delete(id).map_err(|e| e.to_string())?;
        if !ok {
            return Err("clip already gone".into());
        }
        self.clip_preview = None;
        let q = if self.query.trim().is_empty() {
            "/clip".into()
        } else {
            self.query.clone()
        };
        self.set_query(&q);
        if self.selected >= self.hits.len() && !self.hits.is_empty() {
            self.selected = self.hits.len() - 1;
        }
        Ok(Outcome::Listed(self.hits.clone()))
    }

    pub fn go_home(&mut self) {
        self.set_query("");
    }

    pub fn prompt_text(&self) -> String {
        match &self.todo_input {
            Some(TodoInput::Add { buf }) => format!("+ {buf}"),
            Some(TodoInput::Edit { buf, .. }) => format!("edit  {buf}"),
            Some(TodoInput::Progress { buf, .. }) => format!("% {buf}"),
            None if self.query.is_empty() => String::new(),
            None => self.query.clone(),
        }
    }

    pub fn composing(&self) -> bool {
        self.todo_input.is_some()
    }

    fn todo_list_query(&self) -> String {
        let rest = self
            .query
            .trim()
            .split_once(char::is_whitespace)
            .map(|(_, r)| r.trim())
            .unwrap_or("");
        if matches!(rest, "showall" | "all" | "show-all") {
            "/todo showall".into()
        } else {
            "/todo".into()
        }
    }

    fn selected_todo_id(&self) -> Option<i64> {
        self.hits
            .get(self.selected)
            .and_then(|h| h.id.strip_prefix("todo:"))
            .and_then(|s| s.parse().ok())
            .filter(|&id| id > 0)
    }

    pub fn begin_todo_add(&mut self) {
        self.todo_input = Some(TodoInput::Add { buf: String::new() });
    }

    pub fn begin_todo_edit(&mut self) {
        let Some(id) = self.selected_todo_id() else {
            return;
        };
        let title = self
            .hits
            .get(self.selected)
            .map(|h| h.title.clone())
            .unwrap_or_default();
        self.todo_input = Some(TodoInput::Edit { id, buf: title });
    }

    pub fn begin_todo_progress(&mut self) {
        let Some(id) = self.selected_todo_id() else {
            return;
        };
        self.todo_input = Some(TodoInput::Progress {
            id,
            buf: String::new(),
        });
    }

    pub fn cancel_todo_input(&mut self) {
        self.todo_input = None;
    }

    pub fn push_todo_char(&mut self, c: char) {
        if let Some(buf) = self.todo_input.as_mut().map(|t| match t {
            TodoInput::Add { buf } | TodoInput::Edit { buf, .. } | TodoInput::Progress { buf, .. } => {
                buf
            }
        }) {
            buf.push(c);
        }
    }

    pub fn pop_todo_char(&mut self) {
        if let Some(buf) = self.todo_input.as_mut().map(|t| match t {
            TodoInput::Add { buf } | TodoInput::Edit { buf, .. } | TodoInput::Progress { buf, .. } => {
                buf
            }
        }) {
            buf.pop();
        }
    }

    pub fn commit_todo_input(&mut self) -> Result<Outcome, String> {
        let input = self.todo_input.take();
        let q = self.todo_list_query();
        match input {
            Some(TodoInput::Add { buf }) => {
                let title = buf.trim();
                if title.is_empty() {
                    self.set_query(&q);
                    return Ok(Outcome::Listed(self.hits.clone()));
                }
                let t = self
                    .todos
                    .add(title, "", Priority::Medium, Status::Pending, 0, "[]", None)
                    .map_err(|e| e.to_string())?;
                self.set_query(&q);
                Ok(Outcome::Todo(format!("Added task #{}: {}", t.id, t.title)))
            }
            Some(TodoInput::Edit { id, buf }) => {
                let title = buf.trim();
                if !title.is_empty() {
                    let _ = self
                        .todos
                        .update_fields(id, Some(title), None, None, None)
                        .map_err(|e| e.to_string())?;
                }
                self.set_query(&q);
                Ok(Outcome::Listed(self.hits.clone()))
            }
            Some(TodoInput::Progress { id, buf }) => {
                let n = katana_todo::parse_percent(buf.trim()).ok_or("progress 0–100")?;
                let t = self.todos.set_progress(id, n).map_err(|e| e.to_string())?;
                self.set_query(&q);
                Ok(Outcome::Todo(format!(
                    "#{} → {} ({})",
                    t.id,
                    katana_todo::format_progress(t.progress),
                    t.title
                )))
            }
            None => Ok(Outcome::Listed(self.hits.clone())),
        }
    }

    pub fn mark_selected_todo_done(&mut self) -> Result<Outcome, String> {
        let id = self.selected_todo_id().ok_or("no todo selected")?;
        let t = self
            .todos
            .set_status(id, Status::Completed, Some(100))
            .map_err(|e| e.to_string())?;
        let q = self.todo_list_query();
        self.set_query(&q);
        Ok(Outcome::Todo(format!("done #{} {}", t.id, t.title)))
    }

    pub fn apply_clip_history_limit(&mut self, n: usize) -> usize {
        let n = crate::persist::clamp_clip_history_limit(n);
        self.clip_history_limit = n;
        let _ = self.clips.trim_to(n);
        n
    }

    fn after_clip_write(&mut self) {
        let _ = self.clips.trim_to(self.clip_history_limit);
    }

    /// Thumbnail for the overlay (max 280×120), cached per clip id.
    pub fn clip_preview(&mut self, id: i64) -> Option<(u32, u32, Vec<u8>)> {
        if let Some((cid, w, h, ref px)) = self.clip_preview {
            if cid == id {
                return Some((w, h, px.clone()));
            }
        }
        let png = self.clips.get_image(id).ok().flatten()?;
        let (w, h, bgra) = katana_shot::png_to_bgra(&png).ok()?;
        let (tw, th) = fit_thumb(w, h, 280, 120);
        let scaled = if tw == w && th == h {
            bgra
        } else {
            katana_shot::scale_bgra(w, h, &bgra, tw, th).ok()?
        };
        self.clip_preview = Some((id, tw, th, scaled.clone()));
        Some((tw, th, scaled))
    }

    fn run_action(&self, action: &ResolvedAction) -> Result<Outcome, String> {
        if self.dry_run {
            return Ok(Outcome::Ran(format!("{action:?}")));
        }
        Ok(Outcome::Ran(perform(action)?))
    }

    pub fn ingest_clipboard_text(&mut self, text: &str) -> Result<Option<i64>, String> {
        let id = self.clips.record_text(text).map_err(|e| e.to_string())?;
        if id.is_some() {
            self.after_clip_write();
        }
        Ok(id)
    }

    #[allow(dead_code)]
    pub fn ingest_clipboard_image(&mut self, bytes: &[u8]) -> Result<Option<i64>, String> {
        let id = self.clips.record_image(bytes).map_err(|e| e.to_string())?;
        if id.is_some() {
            self.after_clip_write();
        }
        Ok(id)
    }

    /// Finish a capture with already-grabbed physical pixels (tests + UI).
    pub fn complete_capture(
        &mut self,
        mode: ShotMode,
        rect: PhysRect,
        bgra: &[u8],
    ) -> Result<Outcome, String> {
        if matches!(mode, ShotMode::Region | ShotMode::Window | ShotMode::Screen) {
            self.last_region = Some(rect);
        }
        let w = rect.w.max(0) as u32;
        let h = rect.h.max(0) as u32;
        let png = png_from_bgra(w, h, bgra)?;
        let dib = dib_from_bgra(w, h, bgra)?;
        let _ = self.clips.record_image(&png);
        self.after_clip_write();
        Ok(Outcome::Shot {
            width: w,
            height: h,
            png_len: png.len(),
            dib_len: dib.len(),
        })
    }

    #[allow(dead_code)]
    pub fn shot_mode_from_query(&self) -> ShotMode {
        match self.engine.route_str(&self.query) {
            Route::Shot { mode } => mode,
            _ => parse_shot_rest(""),
        }
    }
}

fn fit_thumb(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (1, 1);
    }
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    if w * max_h > h * max_w {
        (max_w, (h * max_w / w).max(1))
    } else {
        ((w * max_h / h).max(1), max_h)
    }
}

pub fn default_user_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Ok(u) = std::env::var("USERPROFILE") {
        roots.push(std::path::PathBuf::from(&u).join("Documents"));
        roots.push(std::path::PathBuf::from(&u).join("Desktop"));
        roots.push(std::path::PathBuf::from(&u).join("Downloads"));
        roots.push(std::path::PathBuf::from(&u).join("src"));
    }
    roots
}

pub fn crawl_roots() -> Vec<std::path::PathBuf> {
    let s = crate::persist::Settings::load(&crate::persist::settings_path());
    let mut roots = default_user_roots();
    for p in &s.crawl_folders {
        let pb = std::path::PathBuf::from(p.trim());
        if pb.as_os_str().is_empty() {
            continue;
        }
        if !roots.iter().any(|r| r == &pb) {
            roots.push(pb);
        }
    }
    roots
}

pub fn rebuild_file_index() -> katana_index::NameIndex {
    katana_index::walk_roots(&crawl_roots())
}

/// Heavy work: crawl folders + Start Menu. Call off the UI thread.
pub fn build_indexes() -> (
    NameIndex,
    Vec<crate::apps::AppEntry>,
    Vec<crate::bookmarks::Bookmark>,
) {
    let files = rebuild_file_index();
    let apps = scan_apps(&default_app_dirs());
    let bookmarks = crate::bookmarks::load_browser_bookmarks();
    (files, apps, bookmarks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use katana_index::rebuild_paths;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "katana-ln-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn launch() -> Launcher {
        let dir = tmp("app");
        std::fs::create_dir_all(&dir).unwrap();
        let files = rebuild_paths(
            'C',
            &[
                katana_index::MftRow {
                    fid: 1,
                    parent_fid: 1,
                    name: ".".into(),
                    is_dir: true,
                },
                katana_index::MftRow {
                    fid: 2,
                    parent_fid: 1,
                    name: "docs".into(),
                    is_dir: true,
                },
                katana_index::MftRow {
                    fid: 3,
                    parent_fid: 2,
                    name: "invoice.pdf".into(),
                    is_dir: false,
                },
            ],
        );
        Launcher::open(&dir.join("t.db"), &dir.join("c.db"), files, true).unwrap()
    }

    #[test]
    fn type_g_rust_executes_google_url() {
        let mut l = launch();
        let hits = l.set_query("g rust crates");
        assert!(!hits.is_empty());
        match l.execute_query("g rust crates").unwrap() {
            Outcome::Ran(s) => {
                assert!(
                    s.contains("google.com/search?q=rust%20crates") || s.contains("OpenUrl"),
                    "{s}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn todo_add_hits_same_store_as_list() {
        let mut l = launch();
        match l.execute_query("/todo add Buy milk").unwrap() {
            Outcome::Todo(s) => {
                assert!(s.contains("Buy milk"), "{s}");
                assert!(s.contains("#1"), "{s}");
            }
            other => panic!("{other:?}"),
        }
        let listed = l.todos.list_active().unwrap();
        assert_eq!(listed[0].title, "Buy milk");
        l.execute_query("/todo done 1").unwrap();
        assert_eq!(l.todos.get(1).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn todo_progress_updates_percent_and_status() {
        let mut l = launch();
        l.execute_query("/todo add Draft docs").unwrap();
        match l.execute_query("/todo progress 1 40").unwrap() {
            Outcome::Todo(s) => assert!(s.contains("40%"), "{s}"),
            other => panic!("{other:?}"),
        }
        let t = l.todos.get(1).unwrap().unwrap();
        assert_eq!(t.progress, 40);
        assert_eq!(t.status, "in_progress");
        l.execute_query("/todo progress 1 100").unwrap();
        let t = l.todos.get(1).unwrap().unwrap();
        assert_eq!((t.progress, t.status.as_str()), (100, "completed"));
    }

    #[test]
    fn todo_overlay_add_edit_done_and_showall() {
        let mut l = launch();
        l.set_query("/todo");
        l.begin_todo_add();
        for c in "Write docs".chars() {
            l.push_todo_char(c);
        }
        l.commit_todo_input().unwrap();
        assert_eq!(l.todos.list_active().unwrap()[0].title, "Write docs");
        assert!(!l.composing());

        l.set_query("/todo");
        l.begin_todo_edit();
        l.pop_todo_char();
        l.pop_todo_char();
        l.pop_todo_char();
        l.pop_todo_char();
        for c in "note".chars() {
            l.push_todo_char(c);
        }
        l.commit_todo_input().unwrap();
        assert_eq!(l.todos.get(1).unwrap().unwrap().title, "Write note");
        assert_eq!(l.todos.get(1).unwrap().unwrap().status, "pending");

        l.set_query("/todo");
        l.mark_selected_todo_done().unwrap();
        l.set_query("/todo");
        assert!(
            !l.hits.iter().any(|h| h.title.contains("Write note")),
            "done items leave the default list: {:?}",
            l.hits
        );
        l.set_query("/todo showall");
        assert!(
            l.hits.iter().any(|h| h.title.contains("Write note")),
            "showall includes completed: {:?}",
            l.hits
        );
        l.set_query("/clip");
        l.go_home();
        assert!(l.query.is_empty());
        assert!(l.hits.iter().any(|h| h.id == "cmd:/todo"));
    }

    #[test]
    fn clip_clear_wipes_history() {
        let mut l = launch();
        assert!(l.ingest_clipboard_text("alpha").unwrap().is_some());
        assert!(l.ingest_clipboard_text("beta").unwrap().is_some());
        l.set_query("/clip");
        assert!(l.hits.iter().any(|h| h.title.contains("alpha")), "{:?}", l.hits);
        l.execute_query("/clip clear").unwrap();
        l.set_query("/clip");
        assert!(
            !l.hits.iter().any(|h| h.title.contains("alpha") || h.title.contains("beta")),
            "{:?}",
            l.hits
        );
        assert!(l.hits.iter().any(|h| h.id == "clip:clear"));
    }

    #[test]
    fn clip_ingest_dedups_and_is_searchable() {
        let mut l = launch();
        assert!(l.ingest_clipboard_text("secret-token").unwrap().is_some());
        assert!(l.ingest_clipboard_text("secret-token").unwrap().is_none());
        l.set_query("/clip secret");
        assert!(
            l.hits.iter().any(|h| h.title.contains("secret-token")),
            "{:?}",
            l.hits
        );
    }

    #[test]
    fn capture_writes_png_dib_and_remembers_region() {
        let mut l = launch();
        let rect = PhysRect {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            dpi: 96,
        };
        let bgra = [0u8, 0, 255, 255, 0, 255, 0, 255];
        let out = l
            .complete_capture(ShotMode::Region, rect, &bgra)
            .unwrap();
        match out {
            Outcome::Shot {
                width,
                height,
                png_len,
                dib_len,
            } => {
                assert_eq!((width, height), (2, 1));
                assert!(png_len > 8);
                assert_eq!(dib_len, 40 + 8);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(l.last_region.unwrap().w, 2);
        let clips = l.clips.list(5).unwrap();
        let img = clips.iter().find(|c| c.kind == "image").expect("image clip");
        assert_eq!((img.width, img.height), (Some(2), Some(1)));
        l.set_query("/clip");
        assert!(
            l.hits
                .iter()
                .any(|h| h.kind == katana_core::HitKind::Clip && h.title.contains("2×1")),
            "{:?}",
            l.hits
        );
    }

    #[test]
    fn file_search_uses_reconstructed_mft_path() {
        let mut l = launch();
        l.set_query("/f invoice");
        assert!(
            l.hits
                .iter()
                .any(|h| crate::search::launch_path(h).as_deref() == Some(r"C:\docs\invoice.pdf")),
            "{:?}",
            l.hits
        );
        match l.execute_current().unwrap() {
            Outcome::Ran(s) => assert!(s.contains("invoice.pdf"), "{s}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shell_elevate_only_on_bang() {
        let mut l = launch();
        match l.execute_query("> echo hi").unwrap() {
            Outcome::Ran(s) => assert!(s.contains("elevate: false") || s.contains("echo hi"), "{s}"),
            other => panic!("{other:?}"),
        }
        match l.execute_query(">! diskpart").unwrap() {
            Outcome::Ran(s) => assert!(s.contains("elevate: true") || s.contains("admin: true"), "{s}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clip_history_limit_drops_oldest_unpinned() {
        let mut l = launch();
        l.clip_history_limit = 3;
        for t in ["one", "two", "three", "four"] {
            assert!(l.ingest_clipboard_text(t).unwrap().is_some());
        }
        let titles: Vec<String> = l
            .clips
            .list(10)
            .unwrap()
            .into_iter()
            .filter_map(|c| c.text)
            .collect();
        assert_eq!(l.clips.count().unwrap(), 3);
        assert!(titles.contains(&"four".into()));
        assert!(!titles.contains(&"one".into()), "{titles:?}");
    }

    #[test]
    fn apply_clip_history_limit_trims_existing() {
        let mut l = launch();
        l.clip_history_limit = 50;
        for i in 0..15 {
            l.ingest_clipboard_text(&format!("item-{i}")).unwrap();
        }
        assert_eq!(l.clips.count().unwrap(), 15);
        assert_eq!(l.apply_clip_history_limit(10), 10);
        assert_eq!(l.clips.count().unwrap(), 10);
        let titles: Vec<String> = l
            .clips
            .list(20)
            .unwrap()
            .into_iter()
            .filter_map(|c| c.text)
            .collect();
        assert!(titles.contains(&"item-14".into()), "{titles:?}");
        assert!(!titles.contains(&"item-0".into()), "{titles:?}");
    }

    #[test]
    fn clip_preview_returns_scaled_thumbnail() {
        let mut l = launch();
        let small = png_from_bgra(40, 10, &vec![80u8; 40 * 10 * 4]).unwrap();
        let sid = l.ingest_clipboard_image(&small).unwrap().unwrap();
        let (tw, th, px) = l.clip_preview(sid).expect("small thumb");
        assert_eq!((tw, th), (40, 10), "do not upscale");
        assert_eq!(px.len(), 40 * 10 * 4);

        let big = png_from_bgra(400, 200, &vec![40u8; 400 * 200 * 4]).unwrap();
        let bid = l.ingest_clipboard_image(&big).unwrap().unwrap();
        let (tw, th, px) = l.clip_preview(bid).expect("big thumb");
        assert!(tw <= 280 && th <= 120, "{tw}x{th}");
        assert_eq!(px.len(), (tw * th * 4) as usize);
        let again = l.clip_preview(bid).unwrap();
        assert_eq!((again.0, again.1), (tw, th));
    }
}
