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
    let ql = query.trim().to_ascii_lowercase();
    let bare = strip_launch_ext(&ql);
    let mut hits = Vec::new();
    for a in apps {
        let stem = a
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut score = eng.score_prepared(&pat, &a.name);
        if !stem.is_empty() && !stem.eq_ignore_ascii_case(&a.name) {
            if let Some(s) = eng.score_prepared(&pat, &stem) {
                score = Some(score.map(|b| b.max(s)).unwrap_or(s));
            }
        }
        let name_l = a.name.to_ascii_lowercase();
        let stem_l = stem.to_ascii_lowercase();
        if !ql.is_empty() && (name_l.contains(&ql) || stem_l.contains(&ql) || name_l.contains(bare) || stem_l.contains(bare))
        {
            score = Some(score.unwrap_or(0.0).max(0.93));
        }
        let Some(score) = score else {
            continue;
        };
        let exact = a.name.eq_ignore_ascii_case(query) || stem.eq_ignore_ascii_case(query);
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

fn push_if_file(out: &mut Vec<AppEntry>, path: PathBuf, name: &str) {
    if path.is_file() {
        out.push(AppEntry {
            name: name.to_string(),
            path,
        });
    }
}

/// Shells and terminals that are not Start Menu shortcuts and not under crawled folders.
pub fn builtin_programs() -> Vec<AppEntry> {
    let mut out = Vec::new();
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let win = PathBuf::from(&windir);
    push_if_file(
        &mut out,
        win.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        "PowerShell",
    );
    push_if_file(&mut out, win.join(r"System32\cmd.exe"), "Command Prompt");
    push_if_file(&mut out, win.join(r"System32\notepad.exe"), "Notepad");
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        push_if_file(
            &mut out,
            PathBuf::from(local).join(r"Microsoft\WindowsApps\wt.exe"),
            "Windows Terminal",
        );
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        push_if_file(
            &mut out,
            PathBuf::from(pf).join(r"PowerShell\7\pwsh.exe"),
            "PowerShell 7",
        );
    }
    out
}

fn strip_launch_ext(q: &str) -> &str {
    let b = q.as_bytes();
    if b.len() > 4 && b[b.len() - 4] == b'.' {
        let ext = &q[q.len() - 3..];
        if ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("lnk") {
            return &q[..q.len() - 4];
        }
    }
    q
}

fn skip_program_dir(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "windowsapps"
            | "winsxs"
            | "jetbrains"
            | "windows kits"
            | "microsoft visual studio"
            | "reference assemblies"
            | "dotnet"
            | "microsoft sdks"
            | "android"
            | "nuget"
            | "msbuild"
    ) || katana_index::skip_dir(name)
}

fn skip_exe_stem(stem: &str) -> bool {
    let s = stem.to_ascii_lowercase();
    s.starts_with("unins")
        || matches!(
            s.as_str(),
            "uninstall"
                | "setup"
                | "update"
                | "updater"
                | "crashpad_handler"
                | "crashreporter"
                | "vc_redist"
        )
}

/// Shallow `.exe` scan. A deep walk of Program Files (JetBrains, SDKs) stalls
/// the index thread and floods `/f` with component binaries.
pub fn scan_installed_exes(cap: usize) -> Vec<AppEntry> {
    let mut dirs = Vec::new();
    if let Ok(pf) = std::env::var("ProgramFiles") {
        dirs.push(PathBuf::from(pf));
    }
    if let Ok(pf) = std::env::var("ProgramFiles(x86)") {
        dirs.push(PathBuf::from(pf));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Programs"));
    }
    let mut out = Vec::new();
    let mut dirs_left = 800u32;
    for d in dirs {
        scan_exe_dir(&d, &mut out, 0, 2, cap, &mut dirs_left);
        if out.len() >= cap || dirs_left == 0 {
            break;
        }
    }
    out
}

fn scan_exe_dir(
    dir: &Path,
    out: &mut Vec<AppEntry>,
    depth: u8,
    max_depth: u8,
    cap: usize,
    dirs_left: &mut u32,
) {
    if out.len() >= cap || depth > max_depth || *dirs_left == 0 {
        return;
    }
    *dirs_left = dirs_left.saturating_sub(1);
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut subdirs = Vec::new();
    for ent in rd.flatten() {
        if out.len() >= cap {
            break;
        }
        let p = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        let ft = match ent.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if !skip_program_dir(&name) {
                subdirs.push(p);
            }
            continue;
        }
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "exe" {
            continue;
        }
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(name);
        if skip_exe_stem(&stem) {
            continue;
        }
        out.push(AppEntry { name: stem, path: p });
    }
    for s in subdirs {
        scan_exe_dir(&s, out, depth + 1, max_depth, cap, dirs_left);
        if out.len() >= cap || *dirs_left == 0 {
            break;
        }
    }
}

pub fn dedup_apps(apps: &mut Vec<AppEntry>) {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| {
        let key = a.path.to_string_lossy().to_ascii_lowercase();
        seen.insert(key)
    });
}

/// Start Menu shortcuts, known shells, and installed executables.
pub fn collect_programs() -> Vec<AppEntry> {
    let mut apps = scan_apps(&default_app_dirs());
    apps.extend(builtin_programs());
    apps.extend(scan_installed_exes(400));
    dedup_apps(&mut apps);
    apps
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

    #[test]
    fn scan_exes_skips_uninstallers_and_indexes_programs() {
        let root = std::env::temp_dir().join(format!(
            "katana-exes-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let app = root.join("Tools").join("bin");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("widget.exe"), b"MZ").unwrap();
        std::fs::write(app.join("unins000.exe"), b"MZ").unwrap();
        std::fs::write(root.join("notes.txt"), b"no").unwrap();
        let mut found = Vec::new();
        let mut dirs_left = 50u32;
        scan_exe_dir(&root, &mut found, 0, 5, 20, &mut dirs_left);
        assert!(
            found.iter().any(|a| a.name == "widget"),
            "exe must be indexed: {found:?}"
        );
        assert!(
            found.iter().all(|a| a.name != "unins000"),
            "uninstallers stay out: {found:?}"
        );
        let hits = search_apps(&found, "widget", 5);
        assert!(hits.iter().any(|h| h.title == "widget"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_matches_by_name_and_powershell_by_stem() {
        let apps = vec![
            AppEntry {
                name: "Windows Terminal".into(),
                path: PathBuf::from(r"C:\Apps\wt.exe"),
            },
            AppEntry {
                name: "PowerShell".into(),
                path: PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            },
        ];
        let term = search_apps(&apps, "terminal", 5);
        assert!(
            term.iter().any(|h| h.title == "Windows Terminal"),
            "{term:?}"
        );
        let ps = search_apps(&apps, "powershell", 5);
        assert!(ps.iter().any(|h| h.title == "PowerShell"), "{ps:?}");
        let wt = search_apps(&apps, "wt", 5);
        assert!(wt.iter().any(|h| h.title == "Windows Terminal"), "{wt:?}");
        let typed = search_apps(&apps, "terminal.exe", 5);
        assert!(
            typed.iter().any(|h| h.title == "Windows Terminal"),
            "typing terminal.exe must still find Windows Terminal: {typed:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn builtin_programs_include_powershell() {
        let apps = builtin_programs();
        assert!(
            apps.iter().any(|a| a.name == "PowerShell"),
            "powershell.exe should launch without a keyword: {apps:?}"
        );
        assert!(apps.iter().any(|a| a.name == "Command Prompt"));
    }
}
