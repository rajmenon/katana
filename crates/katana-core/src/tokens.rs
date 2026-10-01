use crate::url_encode;

/// Max characters kept from a pasted URL argument.
pub const URL_ARG_MAX: usize = 400;

/// Expand shortcut tokens.
///
/// - `$P$` raw argument (commands and paths — not URL templates)
/// - `$U$` URL-encoded argument
/// - `$C$` clipboard text, raw
/// - `$DEF$when_empty$DEF$when_arg` — first segment if no arg, else the rest
pub fn expand_tokens(template: &str, arg: Option<&str>, clipboard: &str) -> String {
    let has_arg = arg.map(|s| !s.is_empty()).unwrap_or(false);
    let body = apply_def(template, has_arg);
    let p = arg.unwrap_or("");
    body.replace("$P$", p)
        .replace("$U$", &url_encode(p))
        .replace("$C$", clipboard)
}

/// URL-keyword expansion. Argument and clipboard are percent-encoded.
/// A URL keyword cannot splice raw text into the address.
pub fn expand_url(template: &str, arg: Option<&str>, clipboard: &str) -> String {
    let has_arg = arg.map(|s| !s.is_empty()).unwrap_or(false);
    let body = apply_def(template, has_arg);
    let p = arg.unwrap_or("");
    body.replace("$P$", &url_encode(p))
        .replace("$U$", &url_encode(p))
        .replace("$C$", &url_encode(clipboard))
}

/// One line of clipboard text safe to append as a URL-keyword argument.
/// Not a command line: controls and hidden characters are dropped, whitespace
/// collapses, and the result is capped.
pub fn sanitize_url_arg(raw: &str) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    let mut pending_space = false;
    let mut started = false;
    for c in raw.chars() {
        if n >= URL_ARG_MAX {
            break;
        }
        if c.is_whitespace() {
            if started {
                pending_space = true;
            }
            continue;
        }
        if c.is_control() || c == '\u{7F}' || is_hidden_paste(c) {
            continue;
        }
        if pending_space {
            if n + 1 >= URL_ARG_MAX {
                break;
            }
            out.push(' ');
            n += 1;
            pending_space = false;
        }
        out.push(c);
        n += 1;
        started = true;
    }
    out
}

fn is_hidden_paste(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

fn apply_def(template: &str, has_arg: bool) -> String {
    const MARK: &str = "$DEF$";
    let Some(start) = template.find(MARK) else {
        return template.to_string();
    };
    let after_first = start + MARK.len();
    let Some(rel) = template[after_first..].find(MARK) else {
        return template.to_string();
    };
    let mid = after_first + rel;
    let default = &template[after_first..mid];
    let rest = &template[mid + MARK.len()..];
    let prefix = &template[..start];
    if has_arg {
        format!("{prefix}{rest}")
    } else {
        format!("{prefix}{default}")
    }
}

/// Split a multi-step command on `||`.
pub fn split_steps(command: &str) -> Vec<&str> {
    command
        .split("||")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_p_u_c() {
        assert_eq!(expand_tokens("echo $P$", Some("hi there"), ""), "echo hi there");
        assert_eq!(
            expand_tokens("https://x/?q=$U$", Some("a b"), ""),
            "https://x/?q=a%20b"
        );
        assert_eq!(expand_tokens("clip=$C$", None, "secret"), "clip=secret");
    }

    #[test]
    fn def_picks_default_or_param_branch() {
        let t = "$DEF$https://google.com$DEF$https://google.com/search?q=$U$";
        assert_eq!(expand_tokens(t, None, ""), "https://google.com");
        assert_eq!(
            expand_tokens(t, Some("rust crates"), ""),
            "https://google.com/search?q=rust%20crates"
        );
    }

    #[test]
    fn splits_parallel_steps() {
        assert_eq!(split_steps("y $P$||g $P$"), vec!["y $P$", "g $P$"]);
    }

    #[test]
    fn url_expansion_encodes_argument_and_clipboard() {
        let t = "https://example.com/search?q=$P$&x=$C$";
        let got = expand_url(t, Some("a&b | calc"), "x y\n");
        assert_eq!(got, "https://example.com/search?q=a%26b%20%7C%20calc&x=x%20y%0A");
        assert!(!got.contains('|'));
    }

    #[test]
    fn sanitize_url_arg_is_one_line_and_capped() {
        assert_eq!(sanitize_url_arg("  rust\r\ncrates\t"), "rust crates");
        assert_eq!(sanitize_url_arg("a\u{202E}b\u{0000}c"), "abc");
        assert_eq!(sanitize_url_arg("   \n"), "");
        let long = "x".repeat(URL_ARG_MAX + 50);
        assert_eq!(sanitize_url_arg(&long).chars().count(), URL_ARG_MAX);
    }
}
