use crate::split_verb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub raw: String,
    pub verb: Option<String>,
    pub rest: String,
}

impl Query {
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        let (verb, rest) = split_verb(trimmed);
        Self {
            raw: trimmed.to_string(),
            verb: verb.map(str::to_string),
            rest: rest.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotMode {
    Region,
    Window,
    Screen,
    Last,
    Delay { secs: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Recents,
    Calc { expr: String },
    Shell { cmdline: String, elevate: bool },
    Url { url: String },
    Keyword { name: String, arg: Option<String> },
    Files { query: String },
    Apps { query: String },
    Clip { query: String },
    Todo { rest: String },
    Shot { mode: ShotMode },
    Shortcuts { query: String },
    Settings,
    EditKeywords,
    EditTodos,
    Unified { query: String },
}

/// Parse `shot` / `ss` remainder into a capture mode.
pub fn parse_shot_rest(rest: &str) -> ShotMode {
    let r = rest.trim();
    if r.is_empty() || r.eq_ignore_ascii_case("region") || r.eq_ignore_ascii_case("r") {
        return ShotMode::Region;
    }
    if r.eq_ignore_ascii_case("win")
        || r.eq_ignore_ascii_case("window")
        || r.eq_ignore_ascii_case("w")
    {
        return ShotMode::Window;
    }
    if r.eq_ignore_ascii_case("screen")
        || r.eq_ignore_ascii_case("full")
        || r.eq_ignore_ascii_case("fullscreen")
        || r.eq_ignore_ascii_case("s")
    {
        return ShotMode::Screen;
    }
    if r.eq_ignore_ascii_case("last") || r.eq_ignore_ascii_case("l") {
        return ShotMode::Last;
    }
    let lower = r.to_ascii_lowercase();
    if lower == "delay" {
        return ShotMode::Delay { secs: 3 };
    }
    if let Some(n) = lower.strip_prefix("delay ") {
        if let Ok(secs) = n.trim().parse::<u32>() {
            return ShotMode::Delay { secs };
        }
    }
    ShotMode::Region
}

fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        return true;
    }
    if t.contains(char::is_whitespace) || t.contains('\\') {
        return false;
    }
    // bare host: example.com or example.com/path
    let host = t.split('/').next().unwrap_or(t);
    if !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// Route a parsed query. `known_keyword` is true when the first token is a defined keyword.
pub fn route(q: &Query, known_keyword: bool) -> Route {
    let raw = q.raw.as_str();
    if raw.is_empty() {
        return Route::Recents;
    }

    if let Some(expr) = raw.strip_prefix('=').or_else(|| raw.strip_prefix('#')) {
        return Route::Calc {
            expr: expr.trim().to_string(),
        };
    }

    if let Some(rest) = raw.strip_prefix(">!") {
        return Route::Shell {
            cmdline: rest.trim().to_string(),
            elevate: true,
        };
    }
    if let Some(rest) = raw.strip_prefix('>') {
        return Route::Shell {
            cmdline: rest.trim().to_string(),
            elevate: false,
        };
    }

    if looks_like_url(raw) {
        let url = if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        return Route::Url { url };
    }

    let verb = q.verb.as_deref().unwrap_or("");
    let verb_l = verb.to_ascii_lowercase();

    // Special commands require a leading `/` so bare text stays search.
    if let Some(name) = verb_l.strip_prefix('/') {
        match name {
            "clip" | "c" => {
                return Route::Clip {
                    query: q.rest.clone(),
                };
            }
            "todo" | "t" => {
                return Route::Todo {
                    rest: q.rest.clone(),
                };
            }
            "shot" | "ss" | "s" => {
                return Route::Shot {
                    mode: parse_shot_rest(&q.rest),
                };
            }
            "f" => {
                return Route::Files {
                    query: q.rest.clone(),
                };
            }
            "apps" | "app" | "a" => {
                return Route::Apps {
                    query: q.rest.clone(),
                };
            }
            "bm" | "b" | "bookmark" | "bookmarks" | "shortcuts" | "sc" => {
                return Route::Shortcuts {
                    query: q.rest.clone(),
                };
            }
            "cmd" => {
                return Route::Shell {
                    cmdline: q.rest.clone(),
                    elevate: false,
                };
            }
            "settings" | "prefs" => {
                return Route::Settings;
            }
            "keywords" | "kw" | "k" | "editkw" => {
                if q.rest.eq_ignore_ascii_case("edit") {
                    return Route::EditKeywords;
                }
                return Route::Shortcuts {
                    query: q.rest.clone(),
                };
            }
            "todos" | "edittodo" => {
                return Route::EditTodos;
            }
            _ => {}
        }
    }

    if known_keyword && !verb.is_empty() {
        let arg = if q.rest.is_empty() {
            None
        } else {
            Some(q.rest.clone())
        };
        return Route::Keyword {
            name: verb.to_string(),
            arg,
        };
    }

    Route::Unified {
        query: raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str, known: bool) -> Route {
        route(&Query::parse(s), known)
    }

    #[test]
    fn routes_empty_to_recents() {
        assert_eq!(r("", false), Route::Recents);
        assert_eq!(r("   ", false), Route::Recents);
    }

    #[test]
    fn routes_calculator() {
        assert_eq!(
            r("=2^9", false),
            Route::Calc {
                expr: "2^9".into()
            }
        );
        assert_eq!(
            r("#123*456", false),
            Route::Calc {
                expr: "123*456".into()
            }
        );
    }

    #[test]
    fn routes_shell_and_elevate() {
        assert_eq!(
            r("> cargo test", false),
            Route::Shell {
                cmdline: "cargo test".into(),
                elevate: false
            }
        );
        assert_eq!(
            r(">! diskpart", false),
            Route::Shell {
                cmdline: "diskpart".into(),
                elevate: true
            }
        );
    }

    #[test]
    fn routes_bare_and_full_urls() {
        assert_eq!(
            r("https://example.com/x", false),
            Route::Url {
                url: "https://example.com/x".into()
            }
        );
        assert_eq!(
            r("github.com/x", false),
            Route::Url {
                url: "https://github.com/x".into()
            }
        );
    }

    #[test]
    fn routes_known_keyword_with_args() {
        assert_eq!(
            r("g rust crates", true),
            Route::Keyword {
                name: "g".into(),
                arg: Some("rust crates".into())
            }
        );
    }

    #[test]
    fn routes_slash_commands_only() {
        assert_eq!(
            r("/clip password", false),
            Route::Clip {
                query: "password".into()
            }
        );
        assert_eq!(
            r("/todo add milk", false),
            Route::Todo {
                rest: "add milk".into()
            }
        );
        assert_eq!(
            r("/t a milk", false),
            Route::Todo {
                rest: "a milk".into()
            }
        );
        assert_eq!(
            r("/shot win", false),
            Route::Shot {
                mode: ShotMode::Window
            }
        );
        assert_eq!(
            r("/ss delay 5", false),
            Route::Shot {
                mode: ShotMode::Delay { secs: 5 }
            }
        );
        assert_eq!(
            r("/f invoice.pdf", false),
            Route::Files {
                query: "invoice.pdf".into()
            }
        );
        assert_eq!(
            r("/apps chrome", false),
            Route::Apps {
                query: "chrome".into()
            }
        );
        assert_eq!(
            r("/bm rust", false),
            Route::Shortcuts {
                query: "rust".into()
            }
        );
        assert_eq!(
            r("/cmd ipconfig", false),
            Route::Shell {
                cmdline: "ipconfig".into(),
                elevate: false
            }
        );
        assert!(
            matches!(r("todo add milk", false), Route::Unified { .. }),
            "bare todo is search, not a command"
        );
        assert_eq!(r("/settings", false), Route::Settings);
        assert!(matches!(r("/keywords", false), Route::Shortcuts { .. }));
        assert_eq!(r("/keywords edit", false), Route::EditKeywords);
        assert_eq!(r("/todos", false), Route::EditTodos);
        assert!(matches!(r("/c", false), Route::Clip { .. }));
        assert!(matches!(r("/s", false), Route::Shot { .. }));
        assert!(matches!(r("/b", false), Route::Shortcuts { .. }));
        assert!(matches!(r("/k", false), Route::Shortcuts { .. }));
        assert!(matches!(r("/shortcuts", false), Route::Shortcuts { .. }));
        assert!(matches!(r("/a", false), Route::Apps { .. }));
    }

    #[test]
    fn unknown_goes_unified() {
        assert_eq!(
            r("notepad", false),
            Route::Unified {
                query: "notepad".into()
            }
        );
    }

    #[test]
    fn parse_shot_modes() {
        assert_eq!(parse_shot_rest(""), ShotMode::Region);
        assert_eq!(parse_shot_rest("window"), ShotMode::Window);
        assert_eq!(parse_shot_rest("screen"), ShotMode::Screen);
        assert_eq!(parse_shot_rest("last"), ShotMode::Last);
        assert_eq!(parse_shot_rest("delay"), ShotMode::Delay { secs: 3 });
    }
}
