//! SQLite todo store — one store, CLI + palette.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TodoError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: #{0}")]
    NotFound(i64),
    #[error("{0}")]
    Msg(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::InProgress => "in_progress",
            Status::Completed => "completed",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Result<Self, TodoError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "p" | "pending" => Ok(Status::Pending),
            "i" | "ip" | "in_progress" | "in progress" | "in-progress" | "progress" => {
                Ok(Status::InProgress)
            }
            "d" | "done" | "completed" => Ok(Status::Completed),
            "c" | "x" | "cancelled" | "canceled" => Ok(Status::Cancelled),
            other => Err(TodoError::Msg(format!("unknown status '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Result<Self, TodoError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "l" => Ok(Priority::Low),
            "medium" | "m" | "med" => Ok(Priority::Medium),
            "high" | "h" => Ok(Priority::High),
            "urgent" | "u" => Ok(Priority::Urgent),
            other => Err(TodoError::Msg(format!("unknown priority '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub progress: i64,
    pub tags: String,
    pub due_date: Option<String>,
    pub position: i64,
}

pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("TODO_DB_PATH") {
        return PathBuf::from(p);
    }
    dirs_data().join("todos.db")
}

fn dirs_data() -> PathBuf {
    if let Ok(p) = std::env::var("APPDATA") {
        return PathBuf::from(p).join("Katana");
    }
    PathBuf::from(".").join(".katana")
}

pub fn legacy_todo_cli_path() -> PathBuf {
    home_dir().join(".todo-cli").join("todos.db")
}

fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("USERPROFILE") {
        return PathBuf::from(h);
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h);
    }
    PathBuf::from(".")
}

/// Copy `src` onto `dest` only if dest is missing. Never deletes `src`.
pub fn import_copy(src: &Path, dest: &Path) -> Result<bool, TodoError> {
    if dest.exists() {
        return Ok(false);
    }
    if !src.exists() {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dest)?;
    Ok(true)
}

/// First-run: copy `~/.todo-cli/todos.db` if dest does not exist. Never deletes source.
pub fn import_legacy_if_needed(dest: &Path) -> Result<bool, TodoError> {
    import_copy(&legacy_todo_cli_path(), dest)
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, TodoError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                priority TEXT NOT NULL DEFAULT 'medium',
                progress INTEGER NOT NULL DEFAULT 0,
                tags TEXT NOT NULL DEFAULT '[]',
                due_date TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT,
                parent_id INTEGER,
                position INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_position ON tasks(position);
            ",
        )?;
        conn.execute_batch("PRAGMA user_version = 1;")?;
        Ok(Self { conn })
    }

    pub fn open_default() -> Result<(Self, PathBuf), TodoError> {
        let path = default_db_path();
        // Only auto-import into the default app path, never a TODO_DB_PATH override.
        if std::env::var("TODO_DB_PATH").is_err() {
            let _ = import_legacy_if_needed(&path);
        }
        Ok((Self::open(&path)?, path))
    }

    fn next_position(&self) -> Result<i64, TodoError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(position), 0) FROM tasks", [], |r| {
                r.get(0)
            })?;
        Ok(n + 1)
    }

    pub fn add(
        &self,
        title: &str,
        description: &str,
        priority: Priority,
        status: Status,
        progress: i64,
        tags: &str,
        due: Option<&str>,
    ) -> Result<Task, TodoError> {
        let pos = self.next_position()?;
        let prog = progress.clamp(0, 100);
        self.conn.execute(
            "INSERT INTO tasks (title, description, status, priority, progress, tags, due_date, position,
             created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'), datetime('now'))",
            params![
                title.trim(),
                description,
                status.as_str(),
                priority.as_str(),
                prog,
                tags,
                due,
                pos
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get(id)?.ok_or(TodoError::NotFound(id))
    }

    pub fn get(&self, id: i64) -> Result<Option<Task>, TodoError> {
        let t = self
            .conn
            .query_row(
                "SELECT id, title, description, status, priority, progress, tags, due_date, position
                 FROM tasks WHERE id = ?1",
                [id],
                row_task,
            )
            .optional()?;
        Ok(t)
    }

    pub fn list_active(&self) -> Result<Vec<Task>, TodoError> {
        self.list_status(&["pending", "in_progress"])
    }

    pub fn list_status(&self, statuses: &[&str]) -> Result<Vec<Task>, TodoError> {
        let placeholders = statuses
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, title, description, status, priority, progress, tags, due_date, position
             FROM tasks WHERE status IN ({placeholders}) ORDER BY position ASC, id ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(statuses.iter().copied());
        let rows = stmt.query_map(params, row_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_all(&self) -> Result<Vec<Task>, TodoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, progress, tags, due_date, position
             FROM tasks ORDER BY position ASC, id ASC",
        )?;
        let rows = stmt.query_map([], row_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Inclusive progress range. Bounds are integers 0–100; the query text is never interpolated.
    pub fn list_progress(&self, lo: i64, hi: i64) -> Result<Vec<Task>, TodoError> {
        if !(0..=100).contains(&lo) || !(0..=100).contains(&hi) || lo > hi {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, progress, tags, due_date, position
             FROM tasks WHERE progress BETWEEN ?1 AND ?2
             ORDER BY position ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![lo, hi], row_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn search(&self, q: &str) -> Result<Vec<Task>, TodoError> {
        let like = format!("%{q}%");
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, progress, tags, due_date, position
             FROM tasks WHERE title LIKE ?1 OR description LIKE ?1
             ORDER BY position ASC, id ASC",
        )?;
        let rows = stmt.query_map([&like], row_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn update_fields(
        &self,
        id: i64,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<Priority>,
        due: Option<Option<&str>>,
    ) -> Result<Task, TodoError> {
        let mut t = self.get(id)?.ok_or(TodoError::NotFound(id))?;
        if let Some(s) = title {
            t.title = s.trim().to_string();
        }
        if let Some(s) = description {
            t.description = s.to_string();
        }
        if let Some(p) = priority {
            t.priority = p.as_str().to_string();
        }
        if let Some(d) = due {
            t.due_date = d.map(str::to_string);
        }
        self.conn.execute(
            "UPDATE tasks SET title=?1, description=?2, priority=?3, due_date=?4,
             updated_at=datetime('now') WHERE id=?5",
            params![t.title, t.description, t.priority, t.due_date, id],
        )?;
        self.get(id)?.ok_or(TodoError::NotFound(id))
    }

    pub fn set_status(&self, id: i64, status: Status, progress: Option<i64>) -> Result<Task, TodoError> {
        let _ = self.get(id)?.ok_or(TodoError::NotFound(id))?;
        let mut prog = progress.unwrap_or(match status {
            Status::Completed => 100,
            Status::InProgress => 1,
            Status::Pending | Status::Cancelled => 0,
        });
        if status == Status::Completed {
            prog = 100;
        }
        let done_at = if matches!(status, Status::Completed | Status::Cancelled) {
            Some("datetime('now')")
        } else {
            None
        };
        if done_at.is_some() {
            self.conn.execute(
                "UPDATE tasks SET status=?1, progress=?2, completed_at=datetime('now'),
                 updated_at=datetime('now') WHERE id=?3",
                params![status.as_str(), prog, id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE tasks SET status=?1, progress=?2, completed_at=NULL,
                 updated_at=datetime('now') WHERE id=?3",
                params![status.as_str(), prog, id],
            )?;
        }
        self.get(id)?.ok_or(TodoError::NotFound(id))
    }

    pub fn set_progress(&self, id: i64, percent: i64) -> Result<Task, TodoError> {
        let p = percent.clamp(0, 100);
        let status = if p >= 100 {
            Status::Completed
        } else if p > 0 {
            Status::InProgress
        } else {
            Status::Pending
        };
        self.set_status(id, status, Some(p))
    }

    pub fn delete(&self, id: i64, renumber: bool) -> Result<bool, TodoError> {
        let n = self.conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        if n > 0 && renumber {
            self.renumber()?;
        }
        Ok(n > 0)
    }

    pub fn clear_completed(&self, renumber: bool) -> Result<usize, TodoError> {
        let n = self.conn.execute(
            "DELETE FROM tasks WHERE status IN ('completed', 'cancelled')",
            [],
        )?;
        if n > 0 && renumber {
            self.renumber()?;
        }
        Ok(n)
    }

    pub fn renumber(&self) -> Result<Vec<(i64, i64)>, TodoError> {
        let tasks = self.list_all()?;
        if tasks.is_empty() {
            return Ok(Vec::new());
        }
        let mapping: Vec<(i64, i64)> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, (i as i64) + 1))
            .collect();
        if mapping.iter().all(|(o, n)| o == n)
            && tasks.iter().enumerate().all(|(i, t)| t.position == (i as i64) + 1)
        {
            return Ok(mapping);
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute("PRAGMA foreign_keys = OFF", [])?;
        const OFF: i64 = 1_000_000;
        for t in &tasks {
            tx.execute(
                "UPDATE tasks SET id = ?, parent_id = NULL WHERE id = ?",
                params![OFF + t.id, t.id],
            )?;
        }
        for (i, t) in tasks.iter().enumerate() {
            let new_id = (i as i64) + 1;
            tx.execute(
                "UPDATE tasks SET id = ?, position = ? WHERE id = ?",
                params![new_id, new_id, OFF + t.id],
            )?;
        }
        tx.execute("PRAGMA foreign_keys = ON", [])?;
        let _ = tx.execute("DELETE FROM sqlite_sequence WHERE name = 'tasks'", []);
        let _ = tx.execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES ('tasks', ?1)",
            params![tasks.len() as i64],
        );
        tx.commit()?;
        Ok(mapping)
    }

    pub fn move_task(&self, id: i64, where_to: &str) -> Result<Task, TodoError> {
        let mut tasks = self.list_all()?;
        let idx = tasks
            .iter()
            .position(|t| t.id == id)
            .ok_or(TodoError::NotFound(id))?;
        let n = tasks.len();
        let w = where_to.trim().to_ascii_lowercase();
        let new_idx = if w == "up" || w == "u" || w == "^" {
            idx.saturating_sub(1)
        } else if w == "down" || w == "d" || w == "v" {
            (idx + 1).min(n.saturating_sub(1))
        } else if w == "top" || w == "t" || w == "first" {
            0
        } else if w == "bottom" || w == "b" || w == "last" {
            n.saturating_sub(1)
        } else if let Ok(pos) = w.parse::<usize>() {
            if pos == 0 {
                return Err(TodoError::Msg("position must be >= 1".into()));
            }
            (pos - 1).min(n.saturating_sub(1))
        } else {
            return Err(TodoError::Msg(format!("bad move target '{where_to}'")));
        };
        let item = tasks.remove(idx);
        tasks.insert(new_idx, item);
        for (i, t) in tasks.iter().enumerate() {
            self.conn.execute(
                "UPDATE tasks SET position = ?1 WHERE id = ?2",
                params![(i as i64) + 1, t.id],
            )?;
        }
        self.renumber()?;
        self.get((new_idx as i64) + 1)?
            .ok_or(TodoError::NotFound((new_idx as i64) + 1))
    }

    pub fn stats(&self) -> Result<String, TodoError> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
        let active: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status IN ('pending','in_progress')",
            [],
            |r| r.get(0),
        )?;
        Ok(format!("total={total} active={active}"))
    }
}

fn row_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        status: r.get(3)?,
        priority: r.get(4)?,
        progress: r.get(5)?,
        tags: r.get(6)?,
        due_date: r.get(7)?,
        position: r.get(8)?,
    })
}

/// Parse a completion percent from user text (`40`, `40%`, `  7 % `).
pub fn parse_percent(s: &str) -> Option<i64> {
    let t = s.trim().trim_end_matches('%').trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<i64>().ok().map(|n| n.clamp(0, 100))
}

pub fn format_progress(percent: i64) -> String {
    format!("{}%", percent.clamp(0, 100))
}

/// Last `N%` in a label such as `#3 · 40% · in progress`.
pub fn percent_from_label(s: &str) -> Option<i64> {
    let end = s.find('%')?;
    let head = &s[..end];
    let start = head
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    head[start..].parse::<i64>().ok().map(|n| n.clamp(0, 100))
}

/// Progress filter introduced by `/` inside the todo blade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressFilter {
    /// `/` with no range — every task, including completed.
    All,
    /// Inclusive bounds, each in 0..=100.
    Range { lo: i64, hi: i64 },
    /// Looked like a filter but was not a range. Match nothing.
    Rejected,
}

/// `/`, `/ 0..10`, `/10..`, `/..50`, `/100`, `/45..85`.
/// `None` when `rest` is not a `/` filter (so other todo verbs still work).
pub fn parse_progress_filter(rest: &str) -> Option<ProgressFilter> {
    let spec = rest.trim().strip_prefix('/')?.trim();
    if spec.is_empty() {
        return Some(ProgressFilter::All);
    }
    Some(match parse_progress_range(spec) {
        Some((lo, hi)) => ProgressFilter::Range { lo, hi },
        None => ProgressFilter::Rejected,
    })
}

fn parse_progress_range(spec: &str) -> Option<(i64, i64)> {
    if let Some(n) = parse_progress_bound(spec) {
        return Some((n, n));
    }
    let (a, b) = spec.split_once("..")?;
    if b.contains("..") {
        return None;
    }
    let a = a.trim();
    let b = b.trim();
    let lo = if a.is_empty() { 0 } else { parse_progress_bound(a)? };
    let hi = if b.is_empty() {
        100
    } else {
        parse_progress_bound(b)?
    };
    if lo > hi {
        None
    } else {
        Some((lo, hi))
    }
}

fn parse_progress_bound(s: &str) -> Option<i64> {
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = s.parse().ok()?;
    if (0..=100).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// Palette helper: parse `todo` rest into a verb + remainder.
pub fn parse_todo_rest(rest: &str) -> (&str, &str) {
    let rest = rest.trim();
    if rest.is_empty() {
        return ("list", "");
    }
    match rest.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (rest, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_db() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("katana-todo-{n}.db"))
    }

    #[test]
    fn add_list_done_rm_renumber_contiguous() {
        let path = tmp_db();
        let store = Store::open(&path).unwrap();
        let a = store
            .add("Alpha", "", Priority::Medium, Status::Pending, 0, "[]", None)
            .unwrap();
        let b = store
            .add("Beta", "", Priority::High, Status::Pending, 0, "[]", None)
            .unwrap();
        let c = store
            .add("Gamma", "", Priority::Low, Status::Pending, 0, "[]", None)
            .unwrap();
        assert_eq!(a.id, 1);
        assert_eq!(b.id, 2);
        assert_eq!(c.id, 3);

        let listed = store.list_active().unwrap();
        assert_eq!(
            listed.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["Alpha", "Beta", "Gamma"]
        );

        store.set_status(2, Status::Completed, Some(100)).unwrap();
        assert_eq!(store.get(2).unwrap().unwrap().status, "completed");

        assert!(store.delete(1, true).unwrap());
        let left = store.list_all().unwrap();
        let ids: Vec<i64> = left.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2], "ids must be 1..N after rm+renumber, got {ids:?}");
        let titles: Vec<_> = left.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Beta", "Gamma"]);

        let d = store
            .add("Delta", "", Priority::Medium, Status::Pending, 0, "[]", None)
            .unwrap();
        store.move_task(d.id, "top").unwrap();
        let after = store.list_all().unwrap();
        let ids: Vec<i64> = after.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(after[0].title, "Delta");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn import_copies_and_leaves_source() {
        let dir = std::env::temp_dir().join(format!(
            "katana-imp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.db");
        fs::write(&src, b"sqlite-placeholder").unwrap();
        let dest = dir.join("dest.db");
        assert!(import_copy(&src, &dest).unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"sqlite-placeholder");
        assert_eq!(fs::read(&src).unwrap(), b"sqlite-placeholder");
        assert!(!import_copy(&src, &dest).unwrap(), "second import is a no-op");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parse_todo_rest_verbs() {
        assert_eq!(parse_todo_rest(""), ("list", ""));
        assert_eq!(parse_todo_rest("add buy milk"), ("add", "buy milk"));
        assert_eq!(parse_todo_rest("a buy milk"), ("a", "buy milk"));
    }

    #[test]
    fn status_parse_accepts_plain_english() {
        assert_eq!(Status::parse("in progress").unwrap(), Status::InProgress);
        assert_eq!(Status::parse("completed").unwrap(), Status::Completed);
        assert_eq!(Status::parse("pending").unwrap(), Status::Pending);
    }

    #[test]
    fn parse_percent_clamps_and_strips_suffix() {
        assert_eq!(parse_percent("40%"), Some(40));
        assert_eq!(parse_percent("  7 "), Some(7));
        assert_eq!(parse_percent("150"), Some(100));
        assert_eq!(parse_percent("-3"), Some(0));
        assert_eq!(parse_percent(""), None);
        assert_eq!(parse_percent("half"), None);
        assert_eq!(percent_from_label("#3 · 40% · in progress"), Some(40));
        assert_eq!(percent_from_label("no percent"), None);
    }

    #[test]
    fn progress_filter_bounds_and_rejects_junk() {
        assert_eq!(parse_progress_filter("/"), Some(ProgressFilter::All));
        assert_eq!(parse_progress_filter("/   "), Some(ProgressFilter::All));
        assert_eq!(
            parse_progress_filter("/ 0..10"),
            Some(ProgressFilter::Range { lo: 0, hi: 10 })
        );
        assert_eq!(
            parse_progress_filter("/10.."),
            Some(ProgressFilter::Range { lo: 10, hi: 100 })
        );
        assert_eq!(
            parse_progress_filter("/..50"),
            Some(ProgressFilter::Range { lo: 0, hi: 50 })
        );
        assert_eq!(
            parse_progress_filter("/50.."),
            Some(ProgressFilter::Range { lo: 50, hi: 100 })
        );
        assert_eq!(
            parse_progress_filter("/100"),
            Some(ProgressFilter::Range { lo: 100, hi: 100 })
        );
        assert_eq!(
            parse_progress_filter("/45..85"),
            Some(ProgressFilter::Range { lo: 45, hi: 85 })
        );
        assert_eq!(
            parse_progress_filter("/ 45 .. 85"),
            Some(ProgressFilter::Range { lo: 45, hi: 85 })
        );
        assert_eq!(parse_progress_filter("/85..45"), Some(ProgressFilter::Rejected));
        assert_eq!(parse_progress_filter("/150"), Some(ProgressFilter::Rejected));
        assert_eq!(parse_progress_filter("/-3"), Some(ProgressFilter::Rejected));
        assert_eq!(
            parse_progress_filter("/; DROP TABLE tasks"),
            Some(ProgressFilter::Rejected)
        );
        assert_eq!(parse_progress_filter("showall"), None);
        assert_eq!(parse_progress_filter("add milk"), None);
        assert_eq!(parse_progress_filter(""), None);
    }

    #[test]
    fn list_progress_includes_completed_and_ignores_title() {
        let path = tmp_db();
        let store = Store::open(&path).unwrap();
        let low = store
            .add("0..10", "", Priority::Medium, Status::Pending, 0, "[]", None)
            .unwrap();
        let mid = store
            .add("middle", "", Priority::Medium, Status::Pending, 0, "[]", None)
            .unwrap();
        let done = store
            .add("shipped", "", Priority::Medium, Status::Pending, 0, "[]", None)
            .unwrap();
        store.set_progress(low.id, 5).unwrap();
        store.set_progress(mid.id, 40).unwrap();
        store.set_progress(done.id, 100).unwrap();
        assert!(
            !store
                .list_active()
                .unwrap()
                .iter()
                .any(|t| t.id == done.id),
            "completed stays off the active list"
        );
        let all = store.list_all().unwrap();
        assert!(all.iter().any(|t| t.id == done.id));
        let low_hits = store.list_progress(0, 10).unwrap();
        assert_eq!(low_hits.len(), 1);
        assert_eq!(low_hits[0].id, low.id);
        let open = store.list_progress(10, 100).unwrap();
        let ids: Vec<_> = open.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![mid.id, done.id]);
        let exact = store.list_progress(100, 100).unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].title, "shipped");
        assert!(store.list_progress(45, 85).unwrap().is_empty());
        assert!(store.list_progress(101, 100).unwrap().is_empty());
        drop(store);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn set_progress_updates_status() {
        let path = tmp_db();
        let store = Store::open(&path).unwrap();
        let t = store
            .add("Ship", "", Priority::Medium, Status::Pending, 0, "[]", None)
            .unwrap();
        assert_eq!(t.progress, 0);
        let mid = store.set_progress(t.id, 40).unwrap();
        assert_eq!(mid.progress, 40);
        assert_eq!(mid.status, "in_progress");
        let done = store.set_progress(t.id, 100).unwrap();
        assert_eq!(done.progress, 100);
        assert_eq!(done.status, "completed");
        let _ = fs::remove_file(path);
    }
}
