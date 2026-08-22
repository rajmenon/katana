use crate::url_encode;

/// Expand shortcut tokens.
///
/// - `$P$` raw argument
/// - `$U$` URL-encoded argument
/// - `$C$` clipboard text
/// - `$DEF$when_empty$DEF$when_arg` — first segment if no arg, else the rest
pub fn expand_tokens(template: &str, arg: Option<&str>, clipboard: &str) -> String {
    let has_arg = arg.map(|s| !s.is_empty()).unwrap_or(false);
    let body = apply_def(template, has_arg);
    let p = arg.unwrap_or("");
    body.replace("$P$", p)
        .replace("$U$", &url_encode(p))
        .replace("$C$", clipboard)
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
}
