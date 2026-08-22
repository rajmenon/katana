use std::path::{Path, PathBuf};

use katana_core::{FuzzyEngine, Hit, HitKind};

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub path: PathBuf,
}

pub fn scan_apps(dirs: &[PathBuf]) -> Vec<AppEntry> {
    let mut out = Vec::new();
    for d in dirs {
        scan_dir(d, &mut out, 0);
    }
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn scan_dir(dir: &Path, out: &mut Vec<AppEntry>, depth: u8) {
    if depth > 8 {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for ent in rd.flatten() {
        let p = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            scan_dir(&p, out, depth + 1);
            continue;
        }
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "lnk" | "exe" | "bat" | "cmd" | "com") {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(name);
            out.push(AppEntry { name: stem, path: p });
        }
    }
}

#[allow(dead_code)]
pub fn default_app_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(app) = std::env::var("APPDATA") {
        dirs.push(PathBuf::from(app).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(prog) = std::env::var("PROGRAMDATA") {
        dirs.push(PathBuf::from(prog).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(user) = std::env::var("USERPROFILE") {
        dirs.push(PathBuf::from(&user).join("Desktop"));
        dirs.push(
            PathBuf::from(&user).join(r"AppData\Roaming\Microsoft\Internet Explorer\Quick Launch"),
        );
    }
    if let Ok(public) = std::env::var("PUBLIC") {
        dirs.push(PathBuf::from(public).join("Desktop"));
    } else {
        dirs.push(PathBuf::from(r"C:\Users\Public\Desktop"));
    }
    // Do not walk PATH — that is the main startup stall.
    dirs
}

pub fn search_apps(apps: &[AppEntry], query: &str, limit: usize) -> Vec<Hit> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut eng = FuzzyEngine::new();
    let pat = FuzzyEngine::prepare(query);
    let mut hits = Vec::new();
    for a in apps {
        let Some(score) = eng.score_prepared(&pat, &a.name) else {
            continue;
        };
        let exact = a.name.eq_ignore_ascii_case(query);
        hits.push(Hit::new(
            format!("app:{}", a.path.display()),
            a.name.clone(),
            "Application",
            if exact { 1.0 } else { score },
            HitKind::App,
        ));
    }
    katana_core::merge_hits(hits, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn scan_and_rank_exe_in_dir() {
        let dir = std::env::temp_dir().join(format!(
            "katana-apps-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("NotepadClone.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(dir.join("Paint.lnk"), b"lnk").unwrap();
        let apps = scan_apps(&[dir.clone()]);
        assert!(
            apps.iter().any(|a| a.name == "NotepadClone"),
            "scan must find the exe stem: {apps:?}"
        );
        assert!(
            apps.iter().any(|a| a.name == "Paint"),
            "scan must index .lnk shortcuts: {apps:?}"
        );
        let hits = search_apps(&apps, "note", 5);
        assert!(
            hits.iter().any(|h| h.title == "NotepadClone"),
            "ranking must surface the scanned app: {hits:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
