//! Browser + Favorites bookmarks, merged with user shortcuts at search time.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub title: String,
    pub url: String,
    pub source: String,
}

pub fn load_browser_bookmarks() -> Vec<Bookmark> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let prefer = default_browser_hint();
    let mut sources: Vec<fn() -> Vec<Bookmark>> = vec![
        load_chrome,
        load_edge,
        load_firefox,
        load_favorites,
    ];
    if prefer == "edge" {
        sources = vec![load_edge, load_chrome, load_firefox, load_favorites];
    } else if prefer == "firefox" {
        sources = vec![load_firefox, load_chrome, load_edge, load_favorites];
    }
    for load in sources {
        for b in load() {
            let key = b.url.to_ascii_lowercase();
            if seen.insert(key) {
                out.push(b);
            }
            if out.len() >= 2500 {
                return out;
            }
        }
    }
    out
}

pub fn default_browser_hint() -> &'static str {
    #[cfg(windows)]
    {
        progid_to_hint(&read_http_progid().unwrap_or_default())
    }
    #[cfg(not(windows))]
    {
        "chrome"
    }
}

pub fn progid_to_hint(progid: &str) -> &'static str {
    let p = progid.to_ascii_lowercase();
    if p.contains("chrome") {
        "chrome"
    } else if p.contains("edge") {
        "edge"
    } else if p.contains("firefox") {
        "firefox"
    } else {
        "chrome"
    }
}

#[cfg(windows)]
fn read_http_progid() -> Option<String> {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ,
    };
    unsafe {
        let mut key = Default::default();
        let st = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\http\\UserChoice"),
            0,
            KEY_READ,
            &mut key,
        );
        if st != ERROR_SUCCESS {
            return None;
        }
        let mut buf = [0u16; 256];
        let mut size = (buf.len() * 2) as u32;
        let got = RegGetValueW(
            key,
            w!(""),
            w!("ProgId"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        if got != ERROR_SUCCESS {
            return None;
        }
        let n = (size as usize / 2).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

fn load_chrome() -> Vec<Bookmark> {
    chrome_family(
        "Chrome",
        local_appdata().join(r"Google\Chrome\User Data\Default\Bookmarks"),
    )
}

fn load_edge() -> Vec<Bookmark> {
    chrome_family(
        "Edge",
        local_appdata().join(r"Microsoft\Edge\User Data\Default\Bookmarks"),
    )
}

fn chrome_family(source: &str, path: PathBuf) -> Vec<Bookmark> {
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_chromium_bookmarks(source, &raw)
}

pub fn parse_chromium_bookmarks(source: &str, json: &str) -> Vec<Bookmark> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(roots) = v.get("roots") {
        for key in ["bookmark_bar", "other", "synced"] {
            if let Some(node) = roots.get(key) {
                walk_chromium(source, node, &mut out);
            }
        }
    }
    out
}

fn walk_chromium(source: &str, node: &serde_json::Value, out: &mut Vec<Bookmark>) {
    if out.len() >= 2500 {
        return;
    }
    let ty = node.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if ty == "url" {
        let url = node.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if url.starts_with("http://") || url.starts_with("https://") {
            let title = node
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(url)
                .to_string();
            out.push(Bookmark {
                title,
                url: url.to_string(),
                source: source.into(),
            });
        }
        return;
    }
    if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for c in children {
            walk_chromium(source, c, out);
        }
    }
}

fn load_firefox() -> Vec<Bookmark> {
    let Some(profile) = firefox_places_path() else {
        return Vec::new();
    };
    if !profile.is_file() {
        return Vec::new();
    }
    read_firefox_places(&profile)
}

fn firefox_places_path() -> Option<PathBuf> {
    let ini = roaming_appdata().join(r"Mozilla\Firefox\profiles.ini");
    let raw = fs::read_to_string(ini).ok()?;
    firefox_default_places(&roaming_appdata().join(r"Mozilla\Firefox"), &raw)
}

pub fn firefox_default_places(root: &Path, profiles_ini: &str) -> Option<PathBuf> {
    let mut path: Option<String> = None;
    let mut is_default = false;
    let mut best: Option<String> = None;
    for line in profiles_ini.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if is_default {
                if let Some(p) = path.take() {
                    best = Some(p);
                }
            }
            path = None;
            is_default = false;
            continue;
        }
        if let Some(v) = line.strip_prefix("Path=") {
            path = Some(v.trim().replace('/', "\\").to_string());
        }
        if line.eq_ignore_ascii_case("Default=1") {
            is_default = true;
        }
    }
    if is_default {
        if let Some(p) = path.take() {
            best = Some(p);
        }
    }
    let rel = best.or(path)?;
    let dir = if Path::new(&rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        root.join(rel)
    };
    Some(dir.join("places.sqlite"))
}

fn read_firefox_places(path: &Path) -> Vec<Bookmark> {
    let uri = format!("file:///{}?immutable=1", path.display().to_string().replace('\\', "/"));
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let Ok(conn) = rusqlite::Connection::open_with_flags(&uri, flags) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT COALESCE(b.title, p.title, p.url), p.url
         FROM moz_bookmarks b
         JOIN moz_places p ON b.fk = p.id
         WHERE b.type = 1 AND p.url LIKE 'http%'
         LIMIT 2500",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(Bookmark {
            title: r.get::<_, String>(0)?,
            url: r.get::<_, String>(1)?,
            source: "Firefox".into(),
        })
    });
    match rows {
        Ok(it) => it.flatten().collect(),
        Err(_) => Vec::new(),
    }
}

fn load_favorites() -> Vec<Bookmark> {
    let mut root = user_profile();
    root.push("Favorites");
    let mut out = Vec::new();
    walk_favorites(&root, &mut out, 0);
    out
}

fn walk_favorites(dir: &Path, out: &mut Vec<Bookmark>, depth: u8) {
    if depth > 8 || out.len() >= 2500 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk_favorites(&p, out, depth + 1);
            continue;
        }
        if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("url")) != Some(true)
        {
            continue;
        }
        if let Some(b) = parse_url_file(&p) {
            out.push(b);
        }
    }
}

pub fn parse_url_file(path: &Path) -> Option<Bookmark> {
    let raw = fs::read_to_string(path).ok()?;
    parse_url_contents(
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Favorite".into()),
        &raw,
    )
}

pub fn parse_url_contents(title: String, raw: &str) -> Option<Bookmark> {
    for line in raw.lines() {
        if let Some(url) = line.trim().strip_prefix("URL=") {
            let url = url.trim();
            if url.starts_with("http://") || url.starts_with("https://") {
                return Some(Bookmark {
                    title,
                    url: url.to_string(),
                    source: "Favorites".into(),
                });
            }
        }
    }
    None
}

fn local_appdata() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| user_profile().join("AppData").join("Local"))
}

fn roaming_appdata() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| user_profile().join("AppData").join("Roaming"))
}

fn user_profile() -> PathBuf {
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_json_walks_nested_urls() {
        let json = r#"{
          "roots": {
            "bookmark_bar": {
              "type": "folder",
              "children": [
                {"type":"url","name":"Rust","url":"https://www.rust-lang.org"},
                {"type":"folder","children":[
                  {"type":"url","name":"Docs","url":"https://doc.rust-lang.org"}
                ]}
              ]
            },
            "other": {"type":"folder","children":[]}
          }
        }"#;
        let b = parse_chromium_bookmarks("Chrome", json);
        assert!(b.iter().any(|x| x.title == "Rust" && x.url.contains("rust-lang")));
        assert!(b.iter().any(|x| x.title == "Docs"));
    }

    #[test]
    fn url_file_reads_internet_shortcut() {
        let b = parse_url_contents(
            "Example".into(),
            "[InternetShortcut]\r\nURL=https://example.com/x\r\n",
        )
        .unwrap();
        assert_eq!(b.url, "https://example.com/x");
        assert_eq!(b.source, "Favorites");
    }

    #[test]
    fn firefox_ini_picks_default_profile() {
        let ini = "[Profile0]\nName=old\nPath=Profiles/aa.old\n\n[Profile1]\nName=now\nPath=Profiles/bb.def\nDefault=1\n";
        let root = PathBuf::from(r"C:\ff");
        let p = firefox_default_places(&root, ini).unwrap();
        assert!(p.ends_with(r"Profiles\bb.def\places.sqlite"), "{}", p.display());
    }

    #[test]
    fn progid_maps_common_browsers() {
        assert_eq!(progid_to_hint("ChromeHTML"), "chrome");
        assert_eq!(progid_to_hint("MSEdgeHTM"), "edge");
        assert_eq!(progid_to_hint("FirefoxURL-308046B0AF4A39CB"), "firefox");
    }
}
