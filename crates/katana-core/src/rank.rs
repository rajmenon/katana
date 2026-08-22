use std::cell::RefCell;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HitKind {
    Keyword = 0,
    App = 1,
    Todo = 2,
    Clip = 3,
    File = 4,
    Calc = 5,
    Shell = 6,
    Shot = 7,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub score: f32,
    pub kind: HitKind,
    pub size: Option<u64>,
    pub type_name: Option<String>,
    pub is_dir: bool,
}

impl Hit {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: impl Into<String>,
        score: f32,
        kind: HitKind,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: subtitle.into(),
            score,
            kind,
            size: None,
            type_name: None,
            is_dir: false,
        }
    }
}

/// Reusable nucleo matcher. Building `Matcher`/`Pattern` per filename is the
/// keystroke stall — compile the query once, score many haystacks.
pub struct FuzzyEngine {
    matcher: Matcher,
    buf: Vec<char>,
}

pub struct PreparedQuery {
    pat: Pattern,
}

impl Default for FuzzyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyEngine {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    pub fn prepare(query: &str) -> PreparedQuery {
        PreparedQuery {
            pat: Pattern::new(
                query,
                CaseMatching::Smart,
                Normalization::Smart,
                AtomKind::Fuzzy,
            ),
        }
    }

    pub fn score_prepared(&mut self, query: &PreparedQuery, haystack: &str) -> Option<f32> {
        if haystack.is_empty() {
            return None;
        }
        let hay = Utf32Str::new(haystack, &mut self.buf);
        query.pat.score(hay, &mut self.matcher).map(|s| {
            (s as f32 / 1000.0).clamp(0.0, 1.0)
        })
    }

    pub fn score(&mut self, query: &str, haystack: &str) -> Option<f32> {
        if query.is_empty() {
            return Some(1.0);
        }
        let q = Self::prepare(query);
        self.score_prepared(&q, haystack)
    }
}

/// nucleo fuzzy score in 0..1 (None = no match).
pub fn fuzzy_score(query: &str, haystack: &str) -> Option<f32> {
    thread_local! {
        static ENG: RefCell<FuzzyEngine> = RefCell::new(FuzzyEngine::new());
    }
    ENG.with(|e| e.borrow_mut().score(query, haystack))
}

fn prefix_bonus(query: &str, title: &str) -> f32 {
    let q = query.to_ascii_lowercase();
    let t = title.to_ascii_lowercase();
    if t == q {
        1.0
    } else if t.starts_with(&q) {
        0.7
    } else if t.split_whitespace().any(|w| w.starts_with(&q)) {
        0.4
    } else {
        0.0
    }
}

fn type_priority(kind: HitKind) -> f32 {
    match kind {
        HitKind::Keyword => 1.0,
        HitKind::App => 0.85,
        HitKind::Todo => 0.7,
        HitKind::Clip => 0.55,
        HitKind::File => 0.4,
        HitKind::Calc | HitKind::Shell | HitKind::Shot => 0.9,
    }
}

/// Combine nucleo + prefix + usage + type. Deterministic.
pub fn score_hit(
    query: &str,
    title: &str,
    kind: HitKind,
    frequency: u32,
    recency: f32,
) -> f32 {
    let nucleo = fuzzy_score(query, title).unwrap_or(0.0);
    let prefix = prefix_bonus(query, title);
    let freq = (frequency as f32 / (frequency as f32 + 8.0)).clamp(0.0, 1.0);
    let rec = recency.clamp(0.0, 1.0);
    0.45 * nucleo + 0.25 * prefix + 0.15 * freq + 0.10 * rec + 0.05 * type_priority(kind)
}

/// Merge provider hits: keep those that match, sort by score desc then title then id.
pub fn merge_hits(mut hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_keyword_outranks_file() {
        let q = "g";
        let kw = score_hit(q, "g", HitKind::Keyword, 0, 0.0);
        let file = score_hit(q, "g", HitKind::File, 0, 0.0);
        assert!(kw > file, "{kw} vs {file}");
    }

    #[test]
    fn merge_is_deterministic_and_prefers_score() {
        let hits = vec![
            Hit::new("b", "beta", "", 0.5, HitKind::App),
            Hit::new("a", "alpha", "", 0.9, HitKind::App),
            Hit::new("c", "alpha", "", 0.9, HitKind::App),
        ];
        let m = merge_hits(hits, 10);
        assert_eq!(m[0].id, "a"); // same score as c, title alpha, id a < c
        assert_eq!(m[1].id, "c");
        assert_eq!(m[2].id, "b");
    }

    #[test]
    fn fuzzy_matches_subsequence() {
        assert!(fuzzy_score("nte", "notepad").is_some());
        assert!(fuzzy_score("zzz", "notepad").is_none());
    }
}
