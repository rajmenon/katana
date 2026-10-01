//! Resident palette: type → route → execute. Owns todo + clip + index.

use katana_clip::ClipStore;
use katana_core::{
    find_keyword, parse_shot_rest, resolve_keyword, Hit, ResolvedAction, Route, ShotMode,
};
use katana_index::NameIndex;
use katana_shot::{dib_from_bgra, png_from_bgra, PhysRect};
use katana_todo::{parse_todo_rest, Priority, Status, Store as TodoStore};

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
    /// Clipboard already set; UI restores the previous window and sends Ctrl+V.
    Paste,
    /// Shell command finished with a non-zero exit (overlay should blink).
    CmdFailed { exit: i32, cmdline: String },
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
    pub file_sort: crate::search::FileSort,
    pub file_sort_desc: bool,
    pub file_scroll: usize,
    /// Char index in the editable prompt (query, or todo compose buffer).
    pub caret: usize,
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
            file_sort: crate::search::FileSort::Score,
            file_sort_desc: false,
            file_scroll: 0,
            caret: 0,
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
            file_sort: crate::search::FileSort::Score,
            file_sort_desc: false,
            file_scroll: 0,
            caret: 0,
            clip_preview: None,
        })
    }

    fn search_limit(&self, raw: &str) -> usize {
        if crate::search::is_file_blade(raw) {
            crate::search::FILE_SEARCH_LIMIT
        } else {
            24
        }
    }

    pub fn set_query(&mut self, raw: &str) -> Vec<Hit> {
        self.set_query_keep_caret(raw, raw.chars().count())
    }

    pub fn set_query_keep_caret(&mut self, raw: &str, caret: usize) -> Vec<Hit> {
        self.todo_input = None;
        self.query = raw.to_string();
        self.caret = crate::search::clamp_caret(self.query.chars().count(), caret);
        let limit = self.search_limit(raw);
        let hits = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.engine
                .search(raw, Some(&self.todos), Some(&self.clips), limit)
                .1
        }))
        .unwrap_or_default();
        self.hits = hits;
        self.selected = 0;
        self.file_scroll = 0;
        self.apply_file_sort();
        self.ensure_file_scroll();
        self.hits.clone()
    }

    fn apply_file_sort(&mut self) {
        if !crate::search::is_file_blade(&self.query) {
            return;
        }
        if self.file_sort == crate::search::FileSort::Score && !self.file_sort_desc {
            return;
        }
        crate::search::sort_file_hits(&mut self.hits, self.file_sort, self.file_sort_desc);
    }

    pub fn cycle_file_sort(&mut self, col: crate::search::FileSort) {
        if self.file_sort == col {
            self.file_sort_desc = !self.file_sort_desc;
        } else {
            self.file_sort = col;
            self.file_sort_desc = matches!(
                col,
                crate::search::FileSort::Modified | crate::search::FileSort::Size
            );
        }
        self.apply_file_sort();
        self.selected = 0;
        self.file_scroll = 0;
        self.ensure_file_scroll();
    }

    pub fn ensure_file_scroll(&mut self) {
        if !crate::search::is_file_blade(&self.query) {
            self.file_scroll = 0;
            return;
        }
        self.file_scroll = crate::search::file_scroll_keep_visible(
            self.hits.len(),
            crate::search::FILE_PAGE,
            self.selected,
            self.file_scroll,
        );
    }

    pub fn scroll_files_by(&mut self, delta: i32) {
        if !crate::search::is_file_blade(&self.query) {
            return;
        }
        let vis = crate::search::FILE_PAGE;
        let max_scroll = self.hits.len().saturating_sub(vis);
        if delta < 0 {
            self.file_scroll = self.file_scroll.saturating_sub((-delta) as usize);
        } else {
            self.file_scroll = (self.file_scroll + delta as usize).min(max_scroll);
        }
        if self.selected < self.file_scroll {
            self.selected = self.file_scroll;
        }
        let last_vis = self.file_scroll + vis.saturating_sub(1);
        if self.selected > last_vis && last_vis < self.hits.len() {
            self.selected = last_vis.min(self.hits.len().saturating_sub(1));
        }
    }

    pub fn page_files(&mut self, down: bool) {
        let vis = crate::search::FILE_PAGE;
        if down {
            self.selected = (self.selected + vis).min(self.hits.len().saturating_sub(1));
        } else {
            self.selected = self.selected.saturating_sub(vis);
        }
        self.ensure_file_scroll();
    }

    fn edit_buf(&self) -> String {
        match &self.todo_input {
            Some(TodoInput::Add { buf } | TodoInput::Edit { buf, .. } | TodoInput::Progress { buf, .. }) => {
                buf.clone()
            }
            None => self.query.clone(),
        }
    }

    fn write_edit_buf(&mut self, s: String, caret: usize) {
        let n = s.chars().count();
        self.caret = crate::search::clamp_caret(n, caret);
        match &mut self.todo_input {
            Some(TodoInput::Add { buf } | TodoInput::Edit { buf, .. } | TodoInput::Progress { buf, .. }) => {
                *buf = s;
            }
            None => {
                self.set_query_keep_caret(&s, self.caret);
            }
        }
    }

    pub fn insert_at_caret(&mut self, ch: char) {
        let (s, c) = crate::search::insert_at(&self.edit_buf(), self.caret, ch);
        self.write_edit_buf(s, c);
    }

    fn insert_str_at_caret(&mut self, extra: &str) {
        let (s, c) = crate::search::insert_str_at(&self.edit_buf(), self.caret, extra);
        self.write_edit_buf(s, c);
    }

    /// Paste clipboard text after a URL keyword, or into a todo compose field.
    /// Does not run the text. Returns false when paste is refused.
    pub fn paste_keyword_arg(&mut self, raw: &str) -> bool {
        if self.composing() {
            return self.paste_into_compose(raw);
        }
        let Some(q) = katana_core::paste_url_arg(&self.query, &self.engine.keywords, raw) else {
            return false;
        };
        self.set_query(&q);
        true
    }

    fn paste_into_compose(&mut self, raw: &str) -> bool {
        let clean = katana_core::sanitize_url_arg(raw);
        if clean.is_empty() || self.todo_input.is_none() {
            return false;
        }
        if matches!(self.todo_input, Some(TodoInput::Progress { .. })) {
            let compact: String = clean
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '%')
                .collect();
            if compact.is_empty()
                || compact.chars().filter(|c| *c == '%').count() > 1
                || katana_todo::parse_percent(&compact).is_none()
            {
                return false;
            }
            self.insert_str_at_caret(&compact);
            return true;
        }
        self.insert_str_at_caret(&clean);
        true
    }

    pub fn backspace_at_caret(&mut self) {
        let (s, c) = crate::search::backspace_at(&self.edit_buf(), self.caret);
        self.write_edit_buf(s, c);
    }

    pub fn delete_at_caret(&mut self) {
        let (s, c) = crate::search::delete_at(&self.edit_buf(), self.caret);
        self.write_edit_buf(s, c);
    }

    pub fn move_caret(&mut self, delta: i32) {
        let n = self.edit_buf().chars().count();
        self.caret = crate::search::move_caret(n, self.caret, delta);
    }

    pub fn caret_home(&mut self) {
        self.caret = 0;
    }

    pub fn caret_end(&mut self) {
        self.caret = self.edit_buf().chars().count();
    }

    /// Char index in `prompt_text()` where the caret is drawn.
    pub fn prompt_caret_index(&self) -> usize {
        let prefix = match &self.todo_input {
            Some(TodoInput::Add { .. }) => 2,
            Some(TodoInput::Edit { .. }) => 6,
            Some(TodoInput::Progress { .. }) => 2,
            None => 0,
        };
        prefix + self.caret
    }

    pub fn set_prompt_caret(&mut self, prompt_index: usize) {
        let prefix = match &self.todo_input {
            Some(TodoInput::Add { .. }) => 2,
            Some(TodoInput::Edit { .. }) => 6,
            Some(TodoInput::Progress { .. }) => 2,
            None => 0,
        };
        let n = self.edit_buf().chars().count();
        self.caret = prompt_index.saturating_sub(prefix).min(n);
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
                if acts.is_empty() {
                    return Err("won't open that".into());
                }
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
            Route::KeepAwake => self.toggle_awake(),
            Route::Clip { query } if crate::search::is_clip_clear_cmd(&query) => {
                self.clear_clips()
            }
            Route::Shortcuts { .. }
            | Route::Launch { .. }
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
            if cmd == "/awake" {
                return self.toggle_awake();
            }
            let q = if cmd == "/f" {
                "/f ".to_string()
            } else if matches!(cmd, "/k" | "/cmd" | "/apps" | "/a" | "/go") {
                "/k ".to_string()
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
        if rest.trim_start().starts_with('/') {
            self.set_query(&format!("/todo {}", rest.trim()));
            return Ok(Outcome::Listed(self.hits.clone()));
        }
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
        if crate::search::clip_hit_is_image(hit) {
            if let Some(id) = id {
                if let Ok(Some(png)) = self.clips.get_image(id) {
                    if let Ok((w, h, bgra)) = katana_shot::png_to_bgra(&png) {
                        if let Ok(dib) = katana_shot::dib_from_bgra(w, h, &bgra) {
                            let _ = crate::capture::set_clipboard_png_dib(&png, &dib);
                        }
                    }
                    return Ok(Outcome::Paste);
                }
            }
        }
        let _ = crate::capture::set_clipboard_text(&hit.title);
        Ok(Outcome::Paste)
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

    fn toggle_awake(&mut self) -> Result<Outcome, String> {
        crate::keepawake::toggle();
        self.set_query("");
        Ok(Outcome::Listed(self.hits.clone()))
    }

    pub fn go_home(&mut self) {
        self.set_query("");
    }

    /// Step toward home. `true` = caller should hide the overlay.
    pub fn go_back(&mut self) -> bool {
        if self.composing() {
            self.cancel_todo_input();
            return false;
        }
        match crate::search::parent_query(&self.query) {
            None => true,
            Some(q) => {
                self.set_query(&q);
                false
            }
        }
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
        } else if rest.starts_with('/') {
            format!("/todo {rest}")
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
        self.caret = 0;
    }

    /// Start a progress filter. `/` stays the search key; `%` still edits one task.
    pub fn begin_todo_filter(&mut self) {
        let rest = self
            .query
            .trim()
            .split_once(char::is_whitespace)
            .map(|(_, r)| r.trim())
            .unwrap_or("");
        if rest.starts_with('/') {
            return;
        }
        self.set_query("/todo /");
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
        self.caret = title.chars().count();
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
        self.caret = 0;
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

    /// Copy the selected file, folder, or program path. No-op clipboard when `dry_run`.
    pub fn copy_selected_path(&self) -> Result<String, String> {
        let hit = self
            .hits
            .get(self.selected)
            .ok_or_else(|| "nothing selected".to_string())?;
        let path = crate::search::launch_path(hit).ok_or_else(|| "no path".to_string())?;
        if !self.dry_run {
            crate::capture::set_clipboard_text(&path)?;
        }
        Ok(path)
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
        match action {
            ResolvedAction::Shell { cmdline, admin } => {
                match crate::exec::run_shell(cmdline, *admin) {
                    Ok(()) => Ok(Outcome::Ran(format!("shell {cmdline}"))),
                    Err(exit) => Ok(Outcome::CmdFailed {
                        exit,
                        cmdline: cmdline.clone(),
                    }),
                }
            }
            other => Ok(Outcome::Ran(perform(other)?)),
        }
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
        if matches!(
            mode,
            ShotMode::Region | ShotMode::Window | ShotMode::Screen | ShotMode::Browser
        ) {
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
    well_known_user_folders()
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

/// Known profile folders so `/f` can open Documents, Downloads, etc.
pub fn well_known_user_folders() -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(u) = std::env::var("USERPROFILE") else {
        return out;
    };
    let home = std::path::PathBuf::from(&u);
    let names = [
        ("Documents", "Documents"),
        ("Downloads", "Downloads"),
        ("Desktop", "Desktop"),
        ("Pictures", "Pictures"),
        ("Videos", "Videos"),
        ("Music", "Music"),
        ("src", "src"),
    ];
    out.push(("Home".into(), home.clone()));
    for (label, sub) in names {
        out.push((label.into(), home.join(sub)));
    }
    out.retain(|(_, p)| p.is_dir());
    out
}

pub fn well_known_folder_hits() -> Vec<Hit> {
    well_known_user_folders()
        .into_iter()
        .map(|(name, path)| folder_hit(&name, &path, 1.0))
        .collect()
}

pub fn matching_folder_hits(query: &str) -> Vec<Hit> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return well_known_folder_hits();
    }
    well_known_user_folders()
        .into_iter()
        .filter_map(|(name, path)| {
            let nl = name.to_ascii_lowercase();
            let score = if nl == q {
                1.0
            } else if nl.starts_with(&q) {
                0.97
            } else if nl.contains(&q) {
                0.8
            } else {
                return None;
            };
            Some(folder_hit(&name, &path, score))
        })
        .collect()
}

fn folder_hit(name: &str, path: &std::path::Path, score: f32) -> Hit {
    let mut h = Hit::new(
        format!("file:{}", path.display()),
        name.to_string(),
        path.display().to_string(),
        score,
        katana_core::HitKind::File,
    );
    h.type_name = Some("File folder".into());
    h.is_dir = true;
    if let Ok(meta) = std::fs::metadata(path) {
        h.modified = katana_index::mtime_unix(&meta);
    }
    h
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

fn user_file_index() -> katana_index::NameIndex {
    let roots = crawl_roots();
    let mut idx = katana_index::build_live_index(&roots);
    for (name, path) in well_known_user_folders() {
        let modified = std::fs::metadata(&path)
            .ok()
            .as_ref()
            .and_then(katana_index::mtime_unix);
        idx.insert_meta(&name, &path.to_string_lossy(), None, true, modified);
    }
    idx
}

fn index_program_exes(idx: &mut katana_index::NameIndex, apps: &[crate::apps::AppEntry]) {
    for a in apps {
        let ext = a
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !ext.eq_ignore_ascii_case("exe") {
            continue;
        }
        let name = a
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.name.clone());
        let meta = std::fs::metadata(&a.path).ok();
        let size = meta.as_ref().and_then(|m| {
            if m.is_file() {
                Some(m.len())
            } else {
                None
            }
        });
        let modified = meta.as_ref().and_then(katana_index::mtime_unix);
        idx.insert_meta(&name, &a.path.to_string_lossy(), size, false, modified);
    }
}

pub fn rebuild_file_index() -> katana_index::NameIndex {
    let mut idx = user_file_index();
    index_program_exes(&mut idx, &crate::apps::collect_programs());
    idx
}

/// Heavy work: crawl folders + programs. Call off the UI thread.
pub fn build_indexes() -> (
    NameIndex,
    Vec<crate::apps::AppEntry>,
    Vec<crate::bookmarks::Bookmark>,
) {
    let apps = crate::apps::collect_programs();
    let mut files = user_file_index();
    index_program_exes(&mut files, &apps);
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
    fn paste_after_keyword_and_todo_progress_filter() {
        let mut l = launch();
        l.set_query("g");
        assert!(l.paste_keyword_arg("rust crates\r\n& calc"));
        assert_eq!(l.query, "g rust crates & calc");
        let ran = l.execute_current().unwrap();
        match ran {
            Outcome::Ran(s) => {
                assert!(s.contains("OpenUrl"), "{s}");
                assert!(s.contains("%26"), "{s}");
                assert!(!s.contains("Shell"), "{s}");
            }
            other => panic!("{other:?}"),
        }
        l.set_query("ps");
        assert!(!l.paste_keyword_arg("Get-Process"));
        assert_eq!(l.query, "ps");
        l.set_query("> notepad");
        assert!(!l.paste_keyword_arg("calc"));
        assert_eq!(l.query, "> notepad");

        l.set_query("/todo");
        l.begin_todo_add();
        l.paste_keyword_arg("Ship it");
        l.commit_todo_input().unwrap();
        l.set_query("/todo");
        l.mark_selected_todo_done().unwrap();
        l.set_query("/todo");
        assert!(
            !l.hits.iter().any(|h| h.title == "Ship it"),
            "completed leaves the default list"
        );
        l.begin_todo_filter();
        assert_eq!(l.query, "/todo /");
        l.insert_at_caret('1');
        l.insert_at_caret('0');
        l.insert_at_caret('0');
        assert_eq!(l.query, "/todo /100");
        assert!(
            l.hits.iter().any(|h| h.title == "Ship it"),
            "filter /100 shows completed: {:?}",
            l.hits
        );
        l.set_query("/todo /0..10");
        assert!(l.hits.iter().all(|h| h.title != "Ship it"));
        l.begin_todo_filter();
        assert_eq!(l.query, "/todo /0..10", "a second slash does not reset the filter");
    }

    #[test]
    fn go_back_steps_through_blade_then_home() {
        let mut l = launch();
        l.set_query("/clip secret");
        assert!(!l.go_back());
        assert_eq!(l.query, "/clip");
        assert!(!l.go_back());
        assert!(l.query.is_empty());
        assert!(l.go_back(), "home Esc hides");
        l.set_query("/todo showall");
        assert!(!l.go_back());
        assert_eq!(l.query, "/todo");
        l.begin_todo_add();
        assert!(l.composing());
        assert!(!l.go_back());
        assert!(!l.composing());
        assert_eq!(l.query, "/todo");
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
    fn clip_enter_dry_run_copies_payload() {
        let mut l = launch();
        l.ingest_clipboard_text("paste-me").unwrap();
        l.set_query("/clip");
        l.selected = l
            .hits
            .iter()
            .position(|h| h.title.contains("paste-me"))
            .unwrap();
        match l.execute_current().unwrap() {
            Outcome::Copied(s) => assert!(s.contains("paste-me"), "{s}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shot_short_commands_need_capture() {
        let mut l = launch();
        match l.execute_query("/sr").unwrap() {
            Outcome::NeedCapture(ShotMode::Region) => {}
            other => panic!("{other:?}"),
        }
        match l.execute_query("/sw").unwrap() {
            Outcome::NeedCapture(ShotMode::Window) => {}
            other => panic!("{other:?}"),
        }
        match l.execute_query("/sf").unwrap() {
            Outcome::NeedCapture(ShotMode::Browser) => {}
            other => panic!("{other:?}"),
        }
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
    fn awake_command_toggles_session_flag() {
        let mut l = launch();
        let start = crate::keepawake::is_on();
        match l.execute_query("/awake").unwrap() {
            Outcome::Listed(hits) => {
                assert!(
                    hits.iter().any(|h| h.id == "cmd:/awake"),
                    "{:?}",
                    hits.iter().map(|h| &h.id).collect::<Vec<_>>()
                );
            }
            other => panic!("{other:?}"),
        }
        assert_ne!(crate::keepawake::is_on(), start);
        crate::keepawake::set(start);
    }

    #[test]
    fn copy_selected_file_path() {
        let mut l = launch();
        l.set_query("/f invoice");
        let path = l.copy_selected_path().unwrap();
        assert!(path.ends_with("invoice.pdf"), "{path}");
        l.selected = 99;
        assert!(l.copy_selected_path().is_err());
    }

    #[test]
    fn file_hotkey_query_has_trailing_space_and_sorts() {
        let mut l = launch();
        l.engine.files.insert_full("zeta.bin", r"C:\docs\zeta.bin", Some(9000), false);
        l.engine.files.insert_full("alpha.bin", r"C:\docs\alpha.bin", Some(50), false);
        l.set_query("/f ");
        assert_eq!(l.query, "/f ");
        l.set_query("/f *.bin");
        l.cycle_file_sort(crate::search::FileSort::Name);
        assert_eq!(l.hits[0].title, "alpha.bin");
        l.cycle_file_sort(crate::search::FileSort::Name);
        assert_eq!(l.hits[0].title, "zeta.bin");
        l.cycle_file_sort(crate::search::FileSort::Size);
        assert_eq!(l.hits[0].title, "zeta.bin", "size defaults to largest first");
    }

    #[test]
    fn caret_edits_middle_of_file_query_and_scrolls() {
        let mut l = launch();
        l.set_query("/f hello");
        assert_eq!(l.caret, 8);
        l.move_caret(-5);
        assert_eq!(l.caret, 3);
        l.insert_at_caret('X');
        assert_eq!(l.query, "/f Xhello");
        l.backspace_at_caret();
        assert_eq!(l.query, "/f hello");
        l.caret_home();
        l.delete_at_caret();
        assert_eq!(l.query, "f hello");
        for i in 0..30 {
            l.engine.files.insert_full(
                &format!("item{i:02}.txt"),
                &format!(r"C:\docs\item{i:02}.txt"),
                Some(i as u64),
                false,
            );
        }
        l.set_query("/f item");
        assert!(l.hits.len() > crate::search::FILE_PAGE);
        l.selected = 12;
        l.ensure_file_scroll();
        assert_eq!(l.file_scroll, 12 + 1 - crate::search::FILE_PAGE);
        l.scroll_files_by(-2);
        assert!(l.file_scroll < 5);
    }

    #[test]
    fn dry_run_shell_does_not_fail() {
        let mut l = launch();
        match l.execute_query("> exit 1").unwrap() {
            Outcome::Ran(s) => assert!(s.contains("exit 1") || s.contains("Shell"), "{s}"),
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
