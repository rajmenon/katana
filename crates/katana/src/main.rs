#![windows_subsystem = "windows"]

mod apps;
mod bookmarks;
mod capture;
mod cli;
mod editors;
mod exec;
mod launcher;
mod persist;
mod search;
mod splash;
mod ui;

use std::env;
use std::time::Duration;

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let stem = std::path::Path::new(&args[0])
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("katana")
        .to_ascii_lowercase();

    if stem == "todo" {
        #[cfg(windows)]
        attach_console();
        std::process::exit(cli::run(&args[1..]));
    }
    if args.get(1).map(String::as_str) == Some("todo") || args.get(1).map(String::as_str) == Some("t")
    {
        #[cfg(windows)]
        attach_console();
        std::process::exit(cli::run(&args[2..]));
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        #[cfg(windows)]
        attach_console();
        println!(
            "Katana — a Japanese swiss knife for Windows. Fast, small, many-talented.\n  katana              tray + Alt+Space\n  katana todo …       todo CLI\n  katana --smoke-gui  create HWND and exit\n"
        );
        return;
    }

    let smoke = args.iter().any(|a| a == "--smoke-gui");
    if let Err(e) = ui::run(smoke) {
        eprintln!("katana: {e}");
        if smoke {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use katana_core::{
        default_keywords, expand_tokens, find_keyword, resolve_keyword, Query, Route,
    };

    #[test]
    fn shipped_router_covers_planned_verbs() {
        use crate::search::Engine;
        use katana_index::NameIndex;
        let e = Engine::new(Vec::new(), NameIndex::new());
        let cases: &[(&str, fn(&Route) -> bool)] = &[
            ("", |r| matches!(r, Route::Recents)),
            ("=1+1", |r| matches!(r, Route::Calc { .. })),
            ("#2*3", |r| matches!(r, Route::Calc { .. })),
            ("> echo hi", |r| matches!(r, Route::Shell { elevate: false, .. })),
            (">! diskpart", |r| matches!(r, Route::Shell { elevate: true, .. })),
            ("https://x.com", |r| matches!(r, Route::Url { .. })),
            ("g rust", |r| matches!(r, Route::Keyword { name, .. } if name == "g")),
            ("/f readme", |r| matches!(r, Route::Files { .. })),
            ("/apps", |r| matches!(r, Route::Apps { .. })),
            ("/clip", |r| matches!(r, Route::Clip { .. })),
            ("/c", |r| matches!(r, Route::Clip { .. })),
            ("/todo", |r| matches!(r, Route::Todo { .. })),
            ("/t", |r| matches!(r, Route::Todo { .. })),
            ("/shot region", |r| matches!(r, Route::Shot { .. })),
            ("/s", |r| matches!(r, Route::Shot { .. })),
            ("/b", |r| matches!(r, Route::Shortcuts { .. })),
            ("/k", |r| matches!(r, Route::Shortcuts { .. })),
            ("/shortcuts", |r| matches!(r, Route::Shortcuts { .. })),
            ("/a", |r| matches!(r, Route::Apps { .. })),
            ("/ss last", |r| matches!(r, Route::Shot { .. })),
            ("/bm rust", |r| matches!(r, Route::Shortcuts { .. })),
            ("/cmd dir", |r| matches!(r, Route::Shell { .. })),
            ("/settings", |r| matches!(r, Route::Settings)),
            ("/keywords", |r| matches!(r, Route::Shortcuts { .. })),
            ("/keywords edit", |r| matches!(r, Route::EditKeywords)),
            ("/todos", |r| matches!(r, Route::EditTodos)),
        ];
        for (input, pred) in cases {
            let r = e.route_str(input);
            assert!(pred(&r), "input {input:?} routed to {r:?}");
        }
    }

    #[test]
    fn keyword_g_expands_via_shipped_tokens() {
        let kws = default_keywords();
        let g = find_keyword(&kws, "g").unwrap();
        let acts = resolve_keyword(g, Some("hello world"), "");
        match &acts[0] {
            katana_core::ResolvedAction::OpenUrl(u) => {
                assert_eq!(u, "https://www.google.com/search?q=hello%20world");
            }
            other => panic!("{other:?}"),
        }
        let raw = expand_tokens(g.url.as_deref().unwrap(), Some("x"), "");
        assert!(raw.contains("search?q=x"));
    }

    #[test]
    fn query_parse_roundtrip() {
        let q = Query::parse("  g rust crates ");
        assert_eq!(q.verb.as_deref(), Some("g"));
        assert_eq!(q.rest, "rust crates");
    }
}
