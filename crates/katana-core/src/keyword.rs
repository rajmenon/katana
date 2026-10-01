use crate::tokens::{expand_tokens, expand_url, sanitize_url_arg, split_steps};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyword {
    pub names: Vec<String>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub args: Option<String>,
    pub steps: Option<Vec<String>>,
    pub command: Option<String>,
}

impl Keyword {
    pub fn matches(&self, verb: &str) -> bool {
        let v = verb.to_ascii_lowercase();
        self.names.iter().any(|n| n.to_ascii_lowercase() == v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAction {
    OpenUrl(String),
    Launch {
        path: String,
        args: Vec<String>,
        admin: bool,
    },
    Shell {
        cmdline: String,
        admin: bool,
    },
    Workflow(Vec<ResolvedAction>),
}

/// Classify an expanded command line into an action.
pub fn classify_command(expanded: &str, elevate: bool) -> ResolvedAction {
    let t = expanded.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        return ResolvedAction::OpenUrl(t.to_string());
    }
    // quoted path or drive-letter / UNC
    if looks_like_path(t) {
        let (path, args) = split_path_args(t);
        return ResolvedAction::Launch {
            path,
            args,
            admin: elevate,
        };
    }
    ResolvedAction::Shell {
        cmdline: t.to_string(),
        admin: elevate,
    }
}

fn looks_like_path(t: &str) -> bool {
    let s = t.trim_start_matches('"');
    s.starts_with("\\\\")
        || (s.len() >= 3
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()[1] == b':'
            && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/'))
}

fn split_path_args(t: &str) -> (String, Vec<String>) {
    let t = t.trim();
    if let Some(rest) = t.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            let path = rest[..end].to_string();
            let args = tokenize(rest[end + 1..].trim());
            return (path, args);
        }
    }
    let mut parts = tokenize(t);
    if parts.is_empty() {
        return (t.to_string(), Vec::new());
    }
    let path = parts.remove(0);
    (path, parts)
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    for c in s.chars() {
        match c {
            '"' => q = !q,
            c if c.is_whitespace() && !q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Resolve a keyword + optional argument into one or more actions.
/// Elevate is never implied here — callers pass it only for `>!` / explicit flag.
pub fn resolve_keyword(kw: &Keyword, arg: Option<&str>, clipboard: &str) -> Vec<ResolvedAction> {
    if let Some(steps) = &kw.steps {
        let inner: Vec<ResolvedAction> = steps
            .iter()
            .flat_map(|s| {
                let exp = expand_tokens(s, arg, clipboard);
                classify_command(&exp, false)
                    .into_workflow()
                    .unwrap_or_else(|a| vec![a])
            })
            .collect();
        return vec![ResolvedAction::Workflow(inner)];
    }

    if let Some(path) = &kw.path {
        let args_t = kw.args.as_deref().unwrap_or("");
        let expanded_args = expand_tokens(args_t, arg, clipboard);
        let args = if expanded_args.is_empty() {
            Vec::new()
        } else {
            tokenize(&expanded_args)
        };
        return vec![ResolvedAction::Launch {
            path: expand_tokens(path, arg, clipboard),
            args,
            admin: false,
        }];
    }

    if let Some(url) = &kw.url {
        let exp = expand_url(url, arg, clipboard);
        if is_safe_http_url(&exp) {
            return vec![ResolvedAction::OpenUrl(exp)];
        }
        // Never fall through to a shell. A URL keyword stays a URL.
        return Vec::new();
    }

    if let Some(cmd) = &kw.command {
        let parts = split_steps(cmd);
        if parts.len() > 1 {
            let acts: Vec<ResolvedAction> = parts
                .into_iter()
                .map(|p| classify_command(&expand_tokens(p, arg, clipboard), false))
                .collect();
            return vec![ResolvedAction::Workflow(acts)];
        }
        return vec![classify_command(&expand_tokens(cmd, arg, clipboard), false)];
    }

    Vec::new()
}

impl ResolvedAction {
    fn into_workflow(self) -> Result<Vec<ResolvedAction>, ResolvedAction> {
        match self {
            ResolvedAction::Workflow(v) => Ok(v),
            other => Err(other),
        }
    }
}

pub fn default_keywords() -> Vec<Keyword> {
    vec![
        Keyword {
            names: vec!["g".into(), "google".into()],
            url: Some(
                "$DEF$https://www.google.com$DEF$https://www.google.com/search?q=$U$".into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["y".into(), "yt".into(), "youtube".into()],
            url: Some(
                "$DEF$https://www.youtube.com$DEF$https://www.youtube.com/results?search_query=$U$"
                    .into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["wiki".into()],
            url: Some(
                "$DEF$https://en.wikipedia.org$DEF$https://en.wikipedia.org/w/index.php?search=$U$"
                    .into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["gh".into(), "github".into()],
            url: Some(
                "$DEF$https://github.com$DEF$https://github.com/search?q=$U$".into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["maps".into()],
            url: Some(
                "$DEF$https://maps.google.com$DEF$https://www.google.com/maps/search/$U$".into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["mail".into()],
            url: Some(
                "$DEF$https://mail.google.com$DEF$https://mail.google.com/mail/#search/$U$".into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["ps".into()],
            url: None,
            path: None,
            args: None,
            steps: None,
            command: Some("powershell -NoProfile -Command $P$".into()),
        },
        Keyword {
            names: vec!["rust".into(), "crates".into()],
            url: Some(
                "$DEF$https://crates.io$DEF$https://crates.io/search?q=$U$".into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
        Keyword {
            names: vec!["docs".into()],
            url: Some(
                "$DEF$https://docs.rs$DEF$https://docs.rs/releases/search?query=$U$".into(),
            ),
            path: None,
            args: None,
            steps: None,
            command: None,
        },
    ]
}

pub fn find_keyword<'a>(kws: &'a [Keyword], verb: &str) -> Option<&'a Keyword> {
    kws.iter().find(|k| k.matches(verb))
}

/// `http`/`https` only. ASCII URL bytes, no controls, quotes, or `|<>\`, at most 2048 bytes.
/// Opened with ShellExecute, not `cmd /C`, so `&` stays inside the address.
pub fn is_safe_http_url(url: &str) -> bool {
    let b = url.as_bytes();
    if b.len() < 8 || b.len() > 2048 {
        return false;
    }
    if !b.iter().copied().all(is_url_byte) {
        return false;
    }
    let rest = if url.len() >= 8 && url.as_bytes()[..8].eq_ignore_ascii_case(b"https://") {
        &url[8..]
    } else if url.len() >= 7 && url.as_bytes()[..7].eq_ignore_ascii_case(b"http://") {
        &url[7..]
    } else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return false;
    }
    host.bytes().any(|c| c.is_ascii_alphanumeric())
}

fn is_url_byte(c: u8) -> bool {
    matches!(
        c,
        b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b':'
            | b'/'
            | b'?'
            | b'#'
            | b'['
            | b']'
            | b'@'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'%'
            | b'='
    )
}

/// Append sanitized clipboard text after a URL keyword. The verb is unchanged.
/// `None` for shell keywords, slash blades, and empty paste.
pub fn paste_url_arg(query: &str, kws: &[Keyword], raw: &str) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() || trimmed.starts_with(['/', '>', '=', '#']) {
        return None;
    }
    let (verb, rest) = crate::split_verb(trimmed);
    let verb = verb?;
    let kw = find_keyword(kws, verb)?;
    // Only the URL branch. Steps and paths still expand `$P$` raw, so paste stays out.
    if kw.url.is_none() || kw.steps.is_some() || kw.path.is_some() {
        return None;
    }
    let arg = sanitize_url_arg(raw);
    if arg.is_empty() {
        return None;
    }
    const MAX: usize = 600;
    let prefix = if rest.is_empty() {
        format!("{verb} ")
    } else {
        format!("{verb} {rest} ")
    };
    let used = prefix.chars().count();
    if used >= MAX {
        return None;
    }
    let arg: String = arg.chars().take(MAX - used).collect();
    if arg.is_empty() {
        return None;
    }
    Some(format!("{prefix}{arg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_keyword_opens_search_url() {
        let g = &default_keywords()[0];
        let acts = resolve_keyword(g, Some("rust crates"), "");
        assert_eq!(
            acts,
            vec![ResolvedAction::OpenUrl(
                "https://www.google.com/search?q=rust%20crates".into()
            )]
        );
        let acts = resolve_keyword(g, None, "");
        assert_eq!(
            acts,
            vec![ResolvedAction::OpenUrl("https://www.google.com".into())]
        );
    }

    #[test]
    fn path_keyword_launches_with_args() {
        let kw = Keyword {
            names: vec!["code".into()],
            url: None,
            path: Some(r"C:\Apps\Code.exe".into()),
            args: Some("$P$".into()),
            steps: None,
            command: None,
        };
        let acts = resolve_keyword(&kw, Some(r"C:\src\Katana"), "");
        assert_eq!(
            acts,
            vec![ResolvedAction::Launch {
                path: r"C:\Apps\Code.exe".into(),
                args: vec![r"C:\src\Katana".into()],
                admin: false,
            }]
        );
    }

    #[test]
    fn multi_step_or_workflow() {
        let kw = Keyword {
            names: vec!["morning".into()],
            url: None,
            path: None,
            args: None,
            steps: None,
            command: Some("https://mail.google.com||https://calendar.google.com".into()),
        };
        let acts = resolve_keyword(&kw, None, "");
        match &acts[0] {
            ResolvedAction::Workflow(steps) => {
                assert_eq!(steps.len(), 2);
                assert!(matches!(&steps[0], ResolvedAction::OpenUrl(u) if u.contains("mail")));
            }
            other => panic!("expected workflow, got {other:?}"),
        }
    }

    #[test]
    fn url_keyword_encodes_paste_and_never_shells() {
        let g = &default_keywords()[0];
        let acts = resolve_keyword(g, Some("rust crates & calc"), "");
        match &acts[0] {
            ResolvedAction::OpenUrl(u) => {
                assert!(u.starts_with("https://www.google.com/search?q="), "{u}");
                assert!(u.contains("%26"), "{u}");
                assert!(is_safe_http_url(u), "{u}");
            }
            other => panic!("{other:?}"),
        }
        let raw = Keyword {
            names: vec!["bad".into()],
            url: Some("$P$".into()),
            path: None,
            args: None,
            steps: None,
            command: None,
        };
        assert!(resolve_keyword(&raw, Some("calc.exe"), "").is_empty());
        let injected = Keyword {
            names: vec!["q".into()],
            url: Some("https://example.com/search?q=$P$&safe=1".into()),
            path: None,
            args: None,
            steps: None,
            command: None,
        };
        let acts = resolve_keyword(&injected, Some("a&b | calc.exe"), "ignored");
        match &acts[0] {
            ResolvedAction::OpenUrl(u) => {
                assert_eq!(
                    u,
                    "https://example.com/search?q=a%26b%20%7C%20calc.exe&safe=1"
                );
                assert!(is_safe_http_url(u));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paste_appends_only_after_a_url_keyword() {
        let kws = default_keywords();
        assert_eq!(
            paste_url_arg("g", &kws, "rust\r\ncrates").as_deref(),
            Some("g rust crates")
        );
        assert_eq!(
            paste_url_arg("g rust", &kws, " crates").as_deref(),
            Some("g rust crates")
        );
        assert_eq!(paste_url_arg("ps", &kws, "Get-Process"), None);
        let mixed = Keyword {
            names: vec!["mix".into()],
            url: Some("https://example.com/?q=$U$".into()),
            path: None,
            args: None,
            steps: Some(vec!["powershell -Command $P$".into()]),
            command: None,
        };
        assert_eq!(paste_url_arg("mix", &[mixed], "calc"), None);
        assert_eq!(paste_url_arg("> dir", &kws, "calc"), None);
        assert_eq!(paste_url_arg("/f", &kws, "secret"), None);
        assert_eq!(paste_url_arg("g", &kws, "\n\t"), None);
        let pasted = paste_url_arg("g", &kws, "> calc & whoami").unwrap();
        assert!(pasted.starts_with("g "));
        assert!(!pasted.contains('\n'));
    }

    #[test]
    fn safe_url_rejects_schemes_and_breaks() {
        assert!(is_safe_http_url("https://example.com/search?q=a%20b"));
        assert!(is_safe_http_url("http://localhost/x"));
        assert!(!is_safe_http_url("javascript:alert(1)"));
        assert!(!is_safe_http_url("https://example.com/a b"));
        assert!(!is_safe_http_url("https://example.com/a|calc"));
        assert!(!is_safe_http_url("https://example.com/a\n"));
        assert!(!is_safe_http_url("https://"));
        assert!(!is_safe_http_url(&format!("https://example.com/{}", "a".repeat(3000))));
    }

    #[test]
    fn classify_does_not_elevate_unless_asked() {
        let a = classify_command(r"C:\Windows\System32\notepad.exe", false);
        match a {
            ResolvedAction::Launch { admin, .. } => assert!(!admin),
            other => panic!("{other:?}"),
        }
        let a = classify_command("ipconfig /all", true);
        match a {
            ResolvedAction::Shell { admin, .. } => assert!(admin),
            other => panic!("{other:?}"),
        }
    }
}
