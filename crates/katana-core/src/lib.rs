//! Query routing, keyword tokens, ranking, and calculator.

mod calc;
mod keyword;
mod query;
mod rank;
mod tokens;

pub use calc::{eval_calc, CalcError};
pub use keyword::{
    classify_command, default_keywords, find_keyword, is_safe_http_url, paste_url_arg,
    resolve_keyword, Keyword, ResolvedAction,
};
pub use query::{route, parse_shot_rest, Query, Route, ShotMode};
pub use rank::{fuzzy_score, merge_hits, score_hit, FuzzyEngine, Hit, HitKind, PreparedQuery};
pub use tokens::{expand_tokens, sanitize_url_arg};

/// First `/verb` of a palette query, without the slash. `"/todo showall"` → `Some("todo")`.
pub fn slash_verb(raw: &str) -> Option<&str> {
    let t = raw.trim();
    let rest = t.strip_prefix('/')?;
    let name = rest.split(char::is_whitespace).next().unwrap_or("");
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Split the first whitespace-separated verb from the remainder.
pub fn split_verb(raw: &str) -> (Option<&str>, &str) {
    let s = raw.trim();
    if s.is_empty() {
        return (None, "");
    }
    match s.find(char::is_whitespace) {
        Some(i) => (Some(&s[..i]), s[i..].trim_start()),
        None => (Some(s), ""),
    }
}

/// Percent-encode for `$U$` (application/x-www-form-urlencoded, space as %20).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_verb_basic() {
        assert_eq!(split_verb("  g rust crates "), (Some("g"), "rust crates"));
        assert_eq!(split_verb("clip"), (Some("clip"), ""));
        assert_eq!(split_verb(""), (None, ""));
        assert_eq!(slash_verb("/todo showall"), Some("todo"));
        assert_eq!(slash_verb("/t"), Some("t"));
        assert_eq!(slash_verb("g rust"), None);
    }

    #[test]
    fn url_encode_space_and_plus() {
        assert_eq!(url_encode("rust crates"), "rust%20crates");
        assert_eq!(url_encode("a+b"), "a%2Bb");
    }
}
