//! Recently used items: apps, files, keywords, commands, and pasted clips.
//!
//! Most recent first, deduped by id, capped. Pure logic; the app crate
//! persists it (`%APPDATA%\Katana\recents.json`).

use crate::rank::{fuzzy_score, Hit, HitKind};

/// Default number of remembered items.
pub const RECENTS_CAP: usize = 50;
/// Recent rows that can jump ahead of normal results while typing.
pub const TYPEAHEAD_MAX: usize = 4;
/// Recent rows outrank every provider hit (provider scores stay near 0..1.1).
const RECENT_FLOOR: f32 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct RecentItem {
    /// Same id the hit had when it was used (`app:…`, `file:…`, `bm:…`,
    /// `clip:N`), or `run:<typed text>` for keywords, URLs, and `>` lines.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub kind: HitKind,
    pub is_dir: bool,
    pub type_name: Option<String>,
    /// Unix seconds of the last use.
    pub used_at: u64,
    pub uses: u32,
}

impl RecentItem {
    pub fn from_hit(h: &Hit, used_at: u64) -> Self {
        Self {
            id: h.id.clone(),
            title: h.title.clone(),
            subtitle: strip_recent_label(&h.subtitle).to_string(),
            kind: h.kind,
            is_dir: h.is_dir,
            type_name: h.type_name.clone(),
            used_at,
            uses: 1,
        }
    }

    /// Palette row. The subtitle says `recent` so it reads differently from a plain hit.
    pub fn to_hit(&self, score: f32) -> Hit {
        let sub = self.subtitle.trim();
        let mut h = Hit::new(
            self.id.clone(),
            self.title.clone(),
            if sub.is_empty() {
                RECENT_LABEL.trim_end_matches([' ', '·']).to_string()
            } else {
                format!("{RECENT_LABEL}{sub}")
            },
            score,
            self.kind,
        );
        h.is_dir = self.is_dir;
        h.type_name = self.type_name.clone();
        h
    }
}

pub const RECENT_LABEL: &str = "recent  ·  ";

/// Remove the `recent  ·  ` prefix a recent row carries.
pub fn strip_recent_label(sub: &str) -> &str {
    sub.strip_prefix(RECENT_LABEL)
        .unwrap_or(if sub == "recent" { "" } else { sub })
}

pub fn is_recent_hit(h: &Hit) -> bool {
    h.subtitle.starts_with(RECENT_LABEL) || h.subtitle == "recent"
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recents {
    items: Vec<RecentItem>,
    cap: usize,
}

impl Default for Recents {
    fn default() -> Self {
        Self::new(RECENTS_CAP)
    }
}

impl Recents {
    pub fn new(cap: usize) -> Self {
        Self {
            items: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Load from disk order-agnostic: newest first, one row per id, capped.
    pub fn from_items(mut items: Vec<RecentItem>, cap: usize) -> Self {
        items.sort_by_key(|x| std::cmp::Reverse(x.used_at));
        let mut out = Self::new(cap);
        for it in items {
            if it.id.is_empty() || out.items.iter().any(|x| x.id == it.id) {
                continue;
            }
            out.items.push(it);
        }
        out.items.truncate(out.cap);
        out
    }

    pub fn items(&self) -> &[RecentItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Record a use: move to the front, bump the count, refresh the label.
    pub fn touch(&mut self, h: &Hit, now: u64) {
        let mut item = RecentItem::from_hit(h, now);
        if let Some(i) = self.items.iter().position(|x| x.id == h.id) {
            let old = self.items.remove(i);
            item.uses = old.uses.saturating_add(1);
            item.used_at = now.max(old.used_at);
        }
        self.items.insert(0, item);
        self.items.truncate(self.cap);
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.items.len();
        self.items.retain(|x| x.id != id);
        self.items.len() != n
    }

    pub fn retain(&mut self, f: impl FnMut(&RecentItem) -> bool) {
        self.items.retain(f);
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut RecentItem> {
        self.items.iter_mut().find(|x| x.id == id)
    }

    /// Home list: newest first.
    pub fn hits(&self, limit: usize, keep: impl Fn(&RecentItem) -> bool) -> Vec<Hit> {
        self.items
            .iter()
            .filter(|x| keep(x))
            .take(limit)
            .enumerate()
            .map(|(i, x)| x.to_hit(RECENT_FLOOR + 1.0 - i as f32 * 0.001))
            .collect()
    }

    /// Type-ahead: recent items whose title matches `query`, best first.
    /// Prefix matches beat fuzzy ones; ties go to the more recent, then the more used.
    pub fn matches(
        &self,
        query: &str,
        limit: usize,
        keep: impl Fn(&RecentItem) -> bool,
    ) -> Vec<Hit> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let ql = q.to_lowercase();
        let n = self.items.len().max(1) as f32;
        let mut scored: Vec<(f32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, x)| keep(x))
            .filter_map(|(i, x)| {
                let tl = x.title.to_lowercase();
                let lead = if tl.starts_with(&ql) {
                    1.0
                } else if tl.split(|c: char| c.is_whitespace() || c == '\\' || c == '.' || c == '-' || c == '_')
                    .any(|w| w.starts_with(&ql))
                {
                    0.6
                } else if q.chars().count() >= 2 {
                    fuzzy_score(q, &x.title).map(|s| 0.2 * s)?
                } else {
                    return None;
                };
                let recency = 1.0 - (i as f32 / n);
                let freq = x.uses as f32 / (x.uses as f32 + 4.0);
                Some((lead + 0.3 * recency + 0.1 * freq, i))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        scored
            .into_iter()
            .take(limit)
            .map(|(s, i)| self.items[i].to_hit(RECENT_FLOOR + s))
            .collect()
    }

    /// Inline completion for the prompt: the rest of the newest recent whose
    /// replay text starts with `query`. Clips are never completed into the prompt.
    pub fn completion(&self, query: &str) -> Option<String> {
        if query.is_empty() || query.trim().is_empty() {
            return None;
        }
        let ql = query.to_lowercase();
        let qn = query.chars().count();
        self.items
            .iter()
            .filter(|x| x.kind != HitKind::Clip)
            .find_map(|x| {
                let text = replay_text(x);
                let tl = text.to_lowercase();
                if tl.starts_with(&ql) && text.chars().count() > qn {
                    Some(text.chars().skip(qn).collect())
                } else {
                    None
                }
            })
    }
}

/// What the prompt holds when this item is replayed by typing.
pub fn replay_text(x: &RecentItem) -> &str {
    x.id.strip_prefix("run:").unwrap_or(&x.title)
}

/// Put matching recents ahead of `hits`, dropping duplicates of the same id.
pub fn prepend_recents(recent: Vec<Hit>, hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut out = recent;
    for h in hits {
        if !out.iter().any(|r| r.id == h.id) {
            out.push(h);
        }
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, title: &str, kind: HitKind) -> Hit {
        Hit::new(id, title, "sub", 1.0, kind)
    }

    #[test]
    fn touch_dedupes_moves_to_front_and_counts() {
        let mut r = Recents::new(10);
        r.touch(&hit("app:code", "Visual Studio Code", HitKind::App), 1);
        r.touch(&hit("run:g rust", "g rust", HitKind::Keyword), 2);
        r.touch(&hit("app:code", "Visual Studio Code", HitKind::App), 3);
        let ids: Vec<_> = r.items().iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["app:code", "run:g rust"]);
        assert_eq!(r.items()[0].uses, 2);
        assert_eq!(r.items()[0].used_at, 3);
    }

    #[test]
    fn cap_drops_oldest() {
        let mut r = Recents::new(3);
        for i in 0..5 {
            r.touch(&hit(&format!("app:{i}"), &format!("a{i}"), HitKind::App), i);
        }
        let ids: Vec<_> = r.items().iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["app:4", "app:3", "app:2"]);
    }

    #[test]
    fn from_items_sorts_dedupes_and_caps() {
        let mk = |id: &str, t: u64| {
            let mut x = RecentItem::from_hit(&hit(id, id, HitKind::App), t);
            x.used_at = t;
            x
        };
        let r = Recents::from_items(vec![mk("a", 1), mk("b", 5), mk("a", 9), mk("c", 3)], 2);
        let ids: Vec<_> = r.items().iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn matches_prefers_prefix_then_recency() {
        let mut r = Recents::new(10);
        r.touch(&hit("file:C:\\notes\\gold.txt", "gold.txt", HitKind::File), 1);
        r.touch(&hit("run:g rust", "g rust", HitKind::Keyword), 2);
        r.touch(&hit("app:git", "Git Bash", HitKind::App), 3);
        let m = r.matches("g", 4, |_| true);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].id, "app:git", "newest prefix match first");
        assert!(m.iter().all(|h| h.score > 2.0));
        assert!(is_recent_hit(&m[0]));
        let m = r.matches("rust", 4, |_| true);
        assert_eq!(m[0].id, "run:g rust", "word prefix matches");
        assert!(r.matches("zzz", 4, |_| true).is_empty());
        let only_apps = r.matches("g", 4, |x| x.kind == HitKind::App);
        assert_eq!(only_apps.len(), 1);
    }

    #[test]
    fn completion_uses_newest_prefix_and_skips_clips() {
        let mut r = Recents::new(10);
        r.touch(&hit("run:g rust lifetimes", "g rust lifetimes", HitKind::Keyword), 1);
        r.touch(&hit("run:g rust", "g rust", HitKind::Keyword), 2);
        r.touch(&hit("clip:7", "g rust secret clip", HitKind::Clip), 3);
        assert_eq!(r.completion("g r").as_deref(), Some("ust"));
        assert_eq!(r.completion("G RUST L").as_deref(), Some("ifetimes"));
        // Exact match on the newest is skipped; the older, longer run still completes.
        assert_eq!(r.completion("g rust").as_deref(), Some(" lifetimes"));
        assert!(r.completion("").is_none());
        assert!(r.completion("x").is_none());
    }

    #[test]
    fn recent_row_label_roundtrips() {
        let mut r = Recents::new(4);
        r.touch(&hit("app:code", "Code", HitKind::App), 1);
        let h = r.hits(10, |_| true).remove(0);
        assert_eq!(h.subtitle, "recent  ·  sub");
        r.touch(&h, 2);
        assert_eq!(r.items()[0].subtitle, "sub", "label is not stacked");
    }

    #[test]
    fn prepend_keeps_recent_first_without_duplicates() {
        let rec = vec![hit("app:a", "A", HitKind::App)];
        let hits = vec![hit("app:b", "B", HitKind::App), hit("app:a", "A", HitKind::App)];
        let out = prepend_recents(rec, hits, 10);
        let ids: Vec<_> = out.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["app:a", "app:b"]);
    }
}
