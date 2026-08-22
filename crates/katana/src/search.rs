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
            Route::Shortcuts { query } => shortcut_hits(self, query, limit),
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
                    let hint = arg.clone().unwrap_or_default();
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
                let mut hits = search_apps(&self.apps, query, limit);
                for f in self.files.search(query, limit) {
                    hits.push(file_hit(f));
                }
                merge_hits(hits, limit)
            }
            Route::Apps { query } => {
                if query.is_empty() {
                    self.apps
                        .iter()
                        .take(limit)
                        .map(app_hit)
                        .collect()
                } else {
                    search_apps(&self.apps, query, limit)
                }
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
                tasks
                    .into_iter()
                    .map(|t| {
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
                    })
                    .collect()
            }
            Route::Settings | Route::EditKeywords | Route::EditTodos => vec![Hit::new(
                "studio",
                "Open editor",
                "Keywords · Todos · Settings",
                1.0,
                HitKind::Keyword,
            )],
            Route::Shot { mode } => vec![Hit::new(
                format!("shot:{mode:?}"),
                format!("Screenshot {mode:?}"),
                "Capture",
                1.0,
                HitKind::Shot,
            )],
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
                merge_hits(hits, limit)
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
        ("/k", Some('k'), "Shortcuts", HitKind::Keyword),
        ("/cmd", None, "Run a shell command", HitKind::Shell),
        ("/f", Some('f'), "Search files", HitKind::File),
        ("/apps", Some('a'), "Apps", HitKind::App),
        ("/settings", None, "Settings", HitKind::Keyword),
        ("g", None, "Google search (no slash)", HitKind::Keyword),
    ]
    .into_iter()
    .map(|(cmd, letter, subtitle, kind)| {
        let title = match letter {
            Some(l) => mark_shortcut(cmd, l),
            None => cmd.to_string(),
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

pub fn is_blade_query(q: &str) -> bool {
    q.trim_start().starts_with('/')
}

pub fn todo_list_mode(q: &str) -> bool {
    if !is_todo_blade(q) {
        return false;
    }
    let rest = q.trim().split_once(char::is_whitespace).map(|(_, r)| r.trim()).unwrap_or("");
    rest.is_empty() || matches!(rest, "showall" | "all" | "show-all")
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
            "+ add    ↵ edit    % progress    alt+v done    esc home"
        });
    }
    if is_clip_blade(q) {
        return Some("↵ paste    del remove    esc home");
    }
    match katana_core::slash_verb(q) {
        Some("shot" | "ss" | "s") => Some("↵ capture    esc home"),
        Some("f") => Some("↵ open    esc home"),
        Some("apps" | "app" | "a") => Some("↵ open    esc home"),
        Some("bm" | "b" | "bookmark" | "bookmarks" | "shortcuts" | "sc" | "k" | "kw" | "keywords") => {
            Some("↵ open    esc home")
        }
        Some("cmd") => Some("↵ run    esc home"),
        Some(_) => Some("esc home"),
        None => Some("↵ open    esc hide    ↑↓ select"),
    }
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
    h
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
        assert!(matches!(e.route_str("/f invoice.pdf"), Route::Files { .. }));
        assert!(matches!(e.route_str("/apps"), Route::Apps { .. }));
        assert!(matches!(e.route_str("/cmd dir"), Route::Shell { .. }));
        assert!(matches!(e.route_str("todo add x"), Route::Unified { .. }));
        assert!(matches!(e.route_str("> cargo test"), Route::Shell { elevate: false, .. }));
        assert!(matches!(e.route_str(">! diskpart"), Route::Shell { elevate: true, .. }));
        assert!(matches!(e.route_str("=2^9"), Route::Calc { .. }));
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
        assert!(hits.iter().any(|h| h.id == "cmd:/k"));
        assert!(!hits.iter().any(|h| h.id == "cmd:/bm" || h.id == "cmd:/keywords"));
        assert!(hits.iter().any(|h| h.id == "cmd:/todo"));
        assert!(blade_footer("", false).is_none());
        assert!(blade_footer("/todo", false).unwrap().contains("alt+v"));
        assert!(blade_footer("/clip", false).unwrap().contains("paste"));
    }

    #[test]
    fn mark_shortcut_brackets_the_letter() {
        assert_eq!(mark_shortcut("/todo", 't'), "/[t]odo");
        assert_eq!(mark_shortcut("/keywords", 'k'), "/[k]eywords");
        assert_eq!(mark_shortcut("/clip", 'c'), "/[c]lip");
        assert_eq!(mark_shortcut("/f", 'f'), "/[f]");
    }

    #[test]
    fn short_aliases_route() {
        let e = eng();
        assert!(matches!(e.route_str("/c"), Route::Clip { .. }));
        assert!(matches!(e.route_str("/s"), Route::Shot { .. }));
        assert!(matches!(e.route_str("/b"), Route::Shortcuts { .. }));
        assert!(matches!(e.route_str("/k"), Route::Shortcuts { .. }));
        assert!(matches!(e.route_str("/a"), Route::Apps { .. }));
        assert!(is_todo_blade("/t showall") && todo_list_mode("/todo showall"));
        assert!(!todo_list_mode("/todo add milk"));
    }

    #[test]
    fn bookmarks_lists_url_keywords() {
        let e = eng();
        let (r, hits) = e.search("/bm", None, None, 16);
        assert!(matches!(r, Route::Shortcuts { .. }));
        assert!(
            hits.iter().any(|h| h.title == "g"),
            "bookmarks should include google shortcut: {hits:?}"
        );
    }
}
