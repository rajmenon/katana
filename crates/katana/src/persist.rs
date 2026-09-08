//! Keywords + settings on disk (`%APPDATA%\Katana`).

use std::fs;
use std::path::{Path, PathBuf};

use katana_core::{default_keywords, Keyword};
use serde::{Deserialize, Serialize};

pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("APPDATA") {
        return PathBuf::from(p).join("Katana");
    }
    PathBuf::from(".").join(".katana")
}

pub fn keywords_path() -> PathBuf {
    data_dir().join("shortcuts.toml")
}

pub fn settings_path() -> PathBuf {
    data_dir().join("config.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    /// Global hotkey that opens `/clip`. Default Win+Alt+C.
    #[serde(default = "default_clip_hotkey")]
    pub clip_hotkey: String,
    /// Global hotkey that opens `/f`. Default Win+Alt+Space.
    #[serde(default = "default_files_hotkey")]
    pub files_hotkey: String,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_true")]
    pub save_shots: bool,
    /// Extra folders to walk for Explorer-style file search (name/size/type).
    #[serde(default)]
    pub crawl_folders: Vec<String>,
    /// Max clipboard history items. Oldest unpinned clips are dropped.
    #[serde(default = "default_clip_history_limit")]
    pub clip_history_limit: usize,
}

pub const CLIP_HISTORY_CHOICES: &[usize] = &[25, 50, 100, 200, 500];

fn default_clip_history_limit() -> usize {
    100
}

pub fn clamp_clip_history_limit(n: usize) -> usize {
    n.clamp(10, 1000)
}

pub fn clip_history_choice_index(n: usize) -> i32 {
    CLIP_HISTORY_CHOICES
        .iter()
        .position(|&c| c == n)
        .or_else(|| {
            CLIP_HISTORY_CHOICES
                .iter()
                .enumerate()
                .min_by_key(|(_, &c)| c.abs_diff(n))
                .map(|(i, _)| i)
        })
        .unwrap_or(2) as i32
}

pub fn clip_history_limit_from_index(i: i32) -> usize {
    CLIP_HISTORY_CHOICES
        .get(i as usize)
        .copied()
        .unwrap_or(default_clip_history_limit())
}

fn default_hotkey() -> String {
    "alt+space".into()
}

fn default_clip_hotkey() -> String {
    "win+alt+c".into()
}

fn default_files_hotkey() -> String {
    "win+alt+space".into()
}

pub const HK_ALT: u32 = 0x0001;
pub const HK_CTRL: u32 = 0x0002;
pub const HK_SHIFT: u32 = 0x0004;
pub const HK_WIN: u32 = 0x0008;
pub const HK_NOREPEAT: u32 = 0x4000;

/// Parse `win+alt+c` / `alt+space` into (RegisterHotKey modifiers, virtual-key).
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let mut mods = 0u32;
    let mut key: Option<u32> = None;
    for part in s.split('+') {
        let p = part.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        match p.as_str() {
            "alt" | "menu" => mods |= HK_ALT,
            "ctrl" | "control" => mods |= HK_CTRL,
            "shift" => mods |= HK_SHIFT,
            "win" | "super" | "meta" | "windows" => mods |= HK_WIN,
            "space" => key = Some(0x20),
            "printscreen" | "prtsc" | "snapshot" => key = Some(0x2C),
            one if one.len() == 1 => {
                let c = one.chars().next()?.to_ascii_uppercase();
                if c.is_ascii_alphanumeric() {
                    key = Some(c as u32);
                }
            }
            _ => return None,
        }
    }
    let vk = key?;
    Some((mods | HK_NOREPEAT, vk))
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            clip_hotkey: default_clip_hotkey(),
            files_hotkey: default_files_hotkey(),
            autostart: false,
            save_shots: true,
            crawl_folders: Vec::new(),
            clip_history_limit: default_clip_history_limit(),
        }
    }
}

/// `Pictures\Katana` — where screenshots land.
pub fn shot_dir() -> PathBuf {
    if let Ok(user) = std::env::var("USERPROFILE") {
        return PathBuf::from(user).join("Pictures").join("Katana");
    }
    data_dir().join("shots")
}

pub fn next_shot_path() -> PathBuf {
    let dir = shot_dir();
    let _ = fs::create_dir_all(&dir);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("Katana-{secs}.png"))
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        let mut s: Self = toml::from_str(&raw).unwrap_or_default();
        s.clip_history_limit = clamp_clip_history_limit(s.clip_history_limit);
        s
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let raw = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, raw).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeywordSer {
    names: Vec<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    steps: Option<Vec<String>>,
}

impl From<&Keyword> for KeywordSer {
    fn from(k: &Keyword) -> Self {
        Self {
            names: k.names.clone(),
            url: k.url.clone(),
            path: k.path.clone(),
            args: k.args.clone(),
            command: k.command.clone(),
            steps: k.steps.clone(),
        }
    }
}

impl From<KeywordSer> for Keyword {
    fn from(k: KeywordSer) -> Self {
        Keyword {
            names: k.names,
            url: k.url,
            path: k.path,
            args: k.args,
            command: k.command,
            steps: k.steps,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct KeywordFile {
    #[serde(default)]
    keyword: Vec<KeywordSer>,
}

pub fn load_keywords(path: &Path) -> Vec<Keyword> {
    let Ok(raw) = fs::read_to_string(path) else {
        return default_keywords();
    };
    match toml::from_str::<KeywordFile>(&raw) {
        Ok(f) if !f.keyword.is_empty() => f.keyword.into_iter().map(Keyword::from).collect(),
        _ => default_keywords(),
    }
}

pub fn save_keywords(path: &Path, kws: &[Keyword]) -> Result<(), String> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let file = KeywordFile {
        keyword: kws.iter().map(KeywordSer::from).collect(),
    };
    let raw = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
    fs::write(path, raw).map_err(|e| e.to_string())
}

pub fn folder_keywords() -> Vec<Keyword> {
    crate::launcher::well_known_user_folders()
        .into_iter()
        .filter_map(|(label, path)| {
            let aliases = match label.as_str() {
                "Documents" => vec!["documents".into(), "mydocs".into()],
                "Downloads" => vec!["downloads".into(), "dl".into()],
                "Desktop" => vec!["desktop".into()],
                "Pictures" => vec!["pictures".into(), "pics".into()],
                "Videos" => vec!["videos".into()],
                "Music" => vec!["music".into()],
                "Home" => vec!["home".into()],
                _ => return None,
            };
            Some(Keyword {
                names: aliases,
                url: None,
                path: Some(path.to_string_lossy().into_owned()),
                args: None,
                steps: None,
                command: None,
            })
        })
        .collect()
}

pub fn merge_folder_keywords(kws: &mut Vec<Keyword>) -> bool {
    let mut added = false;
    for extra in folder_keywords() {
        let clash = extra
            .names
            .iter()
            .any(|n| kws.iter().any(|k| k.matches(n)));
        if !clash {
            kws.push(extra);
            added = true;
        }
    }
    added
}

pub fn ensure_keyword_file() -> Vec<Keyword> {
    let path = keywords_path();
    let legacy = data_dir().join("keywords.toml");
    if !path.exists() && legacy.exists() {
        let _ = fs::rename(&legacy, &path);
    }
    let mut kws = if !path.exists() {
        default_keywords()
    } else {
        load_keywords(&path)
    };
    if merge_folder_keywords(&mut kws) {
        let _ = save_keywords(&path, &kws);
    } else if !path.exists() {
        let _ = save_keywords(&path, &kws);
    }
    kws
}

#[cfg(windows)]
pub fn set_autostart(on: bool) -> Result<(), String> {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegSetValueExW, HKEY_CURRENT_USER, REG_SZ,
    };
    unsafe {
        let mut key = Default::default();
        let status = RegCreateKeyW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            &mut key,
        );
        if status != ERROR_SUCCESS {
            return Err(format!("reg open {status:?}"));
        }
        let result = if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let val = exe.to_string_lossy();
            let bytes: Vec<u8> = val
                .encode_utf16()
                .chain([0])
                .flat_map(|u| u.to_le_bytes())
                .collect();
            let s = RegSetValueExW(key, w!("Katana"), 0, REG_SZ, Some(&bytes));
            if s != ERROR_SUCCESS {
                Err(format!("reg set {s:?}"))
            } else {
                Ok(())
            }
        } else {
            let _ = RegDeleteValueW(key, w!("Katana"));
            Ok(())
        };
        let _ = RegCloseKey(key);
        result
    }
}

#[cfg(not(windows))]
pub fn set_autostart(_on: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "katana-persist-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn keywords_roundtrip_preserves_url_and_names() {
        let dir = tmp("kw");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keywords.toml");
        let mut kws = default_keywords();
        kws.push(Keyword {
            names: vec!["jira".into()],
            url: Some("https://jira.example/browse/$P$".into()),
            path: None,
            args: None,
            steps: None,
            command: None,
        });
        save_keywords(&path, &kws).unwrap();
        let loaded = load_keywords(&path);
        let j = loaded.iter().find(|k| k.matches("jira")).unwrap();
        assert_eq!(j.url.as_deref(), Some("https://jira.example/browse/$P$"));
        let g = loaded.iter().find(|k| k.matches("g")).unwrap();
        assert!(g.url.as_deref().unwrap().contains("google"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_roundtrip() {
        let dir = tmp("cfg");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let s = Settings {
            hotkey: "alt+space".into(),
            clip_hotkey: "win+alt+c".into(),
            files_hotkey: "win+alt+space".into(),
            autostart: true,
            save_shots: true,
            crawl_folders: vec![r"C:\src".into()],
            clip_history_limit: 50,
        };
        s.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded, s);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_files_use_defaults() {
        let p = tmp("missing").join("nope.toml");
        assert!(!load_keywords(&p).is_empty());
        assert_eq!(Settings::load(&p).hotkey, "alt+space");
        assert_eq!(Settings::load(&p).clip_hotkey, "win+alt+c");
        assert_eq!(Settings::load(&p).files_hotkey, "win+alt+space");
        assert!(Settings::default().save_shots);
        assert_eq!(Settings::load(&p).clip_history_limit, 100);
    }

    #[test]
    fn clip_history_limit_clamps_and_maps_combo() {
        assert_eq!(clamp_clip_history_limit(0), 10);
        assert_eq!(clamp_clip_history_limit(5000), 1000);
        assert_eq!(clip_history_limit_from_index(0), 25);
        assert_eq!(clip_history_limit_from_index(2), 100);
        assert_eq!(clip_history_choice_index(100), 2);
        assert_eq!(clip_history_choice_index(50), 1);
        let dir = tmp("clip-lim");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "hotkey = \"alt+space\"\nclip_history_limit = 3\n").unwrap();
        assert_eq!(Settings::load(&path).clip_history_limit, 10);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn merge_folder_keywords_adds_downloads() {
        let mut kws = default_keywords();
        let n = kws.len();
        let _ = merge_folder_keywords(&mut kws);
        if std::env::var("USERPROFILE").is_ok() {
            assert!(kws.len() >= n);
            assert!(
                kws.iter().any(|k| k.matches("downloads") || k.matches("documents")),
                "{:?}",
                kws.iter().flat_map(|k| k.names.clone()).collect::<Vec<_>>()
            );
        }
        let before = kws.len();
        assert!(!merge_folder_keywords(&mut kws));
        assert_eq!(kws.len(), before);
    }

    #[test]
    fn parse_hotkey_win_alt_defaults() {
        let (m, vk) = parse_hotkey("win+alt+c").unwrap();
        assert_eq!(vk, b'C' as u32);
        assert_eq!(m & HK_WIN, HK_WIN);
        assert_eq!(m & HK_ALT, HK_ALT);
        let (m2, vk2) = parse_hotkey("win+alt+space").unwrap();
        assert_eq!(vk2, 0x20);
        assert_eq!(m2 & HK_WIN, HK_WIN);
        let (m3, vk3) = parse_hotkey("alt+space").unwrap();
        assert_eq!(vk3, 0x20);
        assert_eq!(m3 & HK_ALT, HK_ALT);
        assert_eq!(m3 & HK_WIN, 0);
        assert!(parse_hotkey("nope").is_none());
    }

    #[test]
    fn shot_path_is_under_pictures_katana() {
        let p = next_shot_path();
        assert!(p.extension().and_then(|e| e.to_str()) == Some("png"));
        assert!(
            p.to_string_lossy().contains("Katana"),
            "expected Pictures\\\\Katana, got {}",
            p.display()
        );
    }
}
