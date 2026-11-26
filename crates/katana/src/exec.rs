use std::path::{Path, PathBuf};

use katana_core::ResolvedAction;

/// Windows `CreateProcess` flags. Stored on every plan so tests can check them
/// on any host; non-Windows spawn ignores them.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
/// The old runner set this and then waited. Never combine it with a console app.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const _: () = {
    assert!(CREATE_NEW_CONSOLE & CREATE_NO_WINDOW == 0);
    assert!(DETACHED_PROCESS & CREATE_NO_WINDOW == 0);
    assert!(CREATE_NEW_CONSOLE & DETACHED_PROCESS == 0);
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    /// IMAGE_SUBSYSTEM_WINDOWS_CUI, or a batch file.
    Console,
    /// IMAGE_SUBSYSTEM_WINDOWS_GUI.
    Gui,
    /// App-execution alias, or a file with no readable PE header (`wt.exe`).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpawnSpec {
    program: String,
    args: Vec<String>,
    /// Windows creation flags. `CREATE_NO_WINDOW` is never set.
    creation_flags: u32,
    /// Console subsystem: the child gets its own console, stdio not piped.
    console: bool,
    /// Shortcuts (`.lnk`) go through ShellExecute, which also does not wait.
    open_with_shell: bool,
}

/// Perform a resolved action. Returns a human description of what ran.
#[allow(dead_code)]
pub fn perform(action: &ResolvedAction) -> Result<String, String> {
    match action {
        ResolvedAction::OpenUrl(url) => {
            open_url(url)?;
            Ok(format!("open {url}"))
        }
        ResolvedAction::Launch { path, args, admin } => {
            launch(path, args, *admin)?;
            Ok(format!("launch {path}"))
        }
        ResolvedAction::Shell { cmdline, admin } => {
            spawn_shell(cmdline, *admin)?;
            Ok(format!("shell {cmdline}"))
        }
        ResolvedAction::Workflow(steps) => {
            let mut out = Vec::new();
            for s in steps {
                out.push(perform(s)?);
            }
            Ok(out.join(" || "))
        }
    }
}

/// Open a file or folder with the default app / Explorer.
pub fn open_in_shell(path: &std::path::Path) -> Result<(), String> {
    let s = path.as_os_str();
    if s.is_empty() {
        return Err("empty path".into());
    }
    launch(&path.to_string_lossy(), &[], false)
}

fn open_url(url: &str) -> Result<(), String> {
    if !katana_core::is_safe_http_url(url) {
        return Err("refusing to open that url".into());
    }
    #[cfg(windows)]
    {
        return shell_open(url, &[]);
    }
    #[cfg(not(windows))]
    {
        Err("open_url is Windows-only".into())
    }
}

fn launch(path: &str, args: &[String], admin: bool) -> Result<(), String> {
    if admin {
        return shell_runas(path, args);
    }
    #[cfg(windows)]
    {
        return shell_open(path, args);
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new(path);
        cmd.args(args);
        // Dropping the child does not wait, and stdio is not a pipe.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Open a file, folder, .lnk, or exe with the shell — `Command::new` cannot open documents.
#[cfg(windows)]
fn shell_open(path: &str, args: &[String]) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb: Vec<u16> = OsStr::new("open").encode_wide().chain([0]).collect();
    let file_w: Vec<u16> = OsStr::new(path).encode_wide().chain([0]).collect();
    let params = join_params(args);
    let params_w: Vec<u16> = OsStr::new(&params).encode_wide().chain([0]).collect();
    let r = unsafe {
        ShellExecuteW(
            HWND(std::ptr::null_mut()),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            if args.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(params_w.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as usize <= 32 {
        return Err(format!("open failed ({}) {path}", r.0 as usize));
    }
    Ok(())
}

/// Start a command and return as soon as the OS has created the process.
///
/// The caller may be the UI thread: process creation runs on `katana-spawn`
/// and this waits only for that call, never for the child to exit. Stdio is
/// not piped. On Windows a console program (powershell, cmd, wt) gets
/// `CREATE_NEW_CONSOLE`; a GUI program gets `DETACHED_PROCESS`.
pub(crate) fn spawn_shell(cmdline: &str, admin: bool) -> Result<(), String> {
    if admin {
        // `runas` shows UAC. Keep that on the caller so the prompt has a desktop.
        return spawn_shell_here(cmdline, true);
    }
    let cmdline = cmdline.to_string();
    let caller = std::thread::current().id();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("katana-spawn".into())
        .spawn(move || {
            let result = if std::thread::current().id() == caller {
                Err("spawn ran on the caller thread".into())
            } else {
                spawn_shell_here(&cmdline, false)
            };
            let _ = tx.send(result);
        })
        .map_err(|e| format!("spawn thread: {e}"))?;
    rx.recv()
        .unwrap_or_else(|_| Err("spawn thread ended before the process was created".into()))
}

/// Resolve and `CreateProcess` on the current thread. The message loop calls
/// this from `katana-spawn`, not from the window procedure.
pub(crate) fn spawn_shell_here(cmdline: &str, admin: bool) -> Result<(), String> {
    let spec = plan_shell(cmdline)?;
    if admin {
        return shell_runas(&spec.program, &spec.args);
    }
    spawn_spec_here(&spec)
}

fn plan_shell(cmdline: &str) -> Result<SpawnSpec, String> {
    let dirs = search_dirs();
    plan_shell_with(
        cmdline,
        &|token| resolve_program(token, &dirs, &lookup_app_path),
        &image_kind,
    )
}

fn plan_shell_with(
    cmdline: &str,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
    kind_of: &dyn Fn(&Path) -> ImageKind,
) -> Result<SpawnSpec, String> {
    let cmdline = cmdline.trim();
    if cmdline.is_empty() {
        return Err("empty command".into());
    }
    if needs_shell(cmdline) {
        return Ok(shell_spec(cmdline));
    }
    let mut parts = tokenize(cmdline);
    if parts.is_empty() {
        return Err("empty command".into());
    }
    let program = parts.remove(0);
    if is_cmd_builtin(&program) {
        return Ok(shell_spec(cmdline));
    }
    let Some(path) = resolve(&program) else {
        return Err(format!("not found: {program}"));
    };
    let kind = kind_of(&path);
    Ok(spec_for_resolved(path, parts, kind))
}

fn spec_for_resolved(path: PathBuf, args: Vec<String>, kind: ImageKind) -> SpawnSpec {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if is_shell_link(&ext) {
        return SpawnSpec {
            program: path.display().to_string(),
            args,
            creation_flags: 0,
            console: false,
            open_with_shell: true,
        };
    }
    if is_batch(&ext) {
        let mut line = format!("call {}", quote_win(&path.display().to_string()));
        for a in &args {
            line.push(' ');
            line.push_str(&quote_win(a));
        }
        return shell_spec(&line);
    }
    let console = kind != ImageKind::Gui;
    SpawnSpec {
        program: path.display().to_string(),
        args,
        creation_flags: creation_flags(kind),
        console,
        open_with_shell: false,
    }
}

fn creation_flags(kind: ImageKind) -> u32 {
    match kind {
        ImageKind::Gui => DETACHED_PROCESS,
        // Unknown covers `wt.exe` app-execution aliases, which are not a PE.
        ImageKind::Console | ImageKind::Unknown => CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP,
    }
}

fn shell_spec(cmdline: &str) -> SpawnSpec {
    #[cfg(windows)]
    {
        SpawnSpec {
            program: comspec(),
            args: vec!["/C".into(), cmdline.to_string()],
            creation_flags: CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP,
            console: true,
            open_with_shell: false,
        }
    }
    #[cfg(not(windows))]
    {
        SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), cmdline.to_string()],
            creation_flags: 0,
            console: false,
            open_with_shell: false,
        }
    }
}

#[cfg(windows)]
fn comspec() -> String {
    std::env::var("ComSpec").unwrap_or_else(|_| {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        format!(r"{root}\System32\cmd.exe")
    })
}

fn spawn_spec_here(spec: &SpawnSpec) -> Result<(), String> {
    #[cfg(windows)]
    {
        if spec.open_with_shell {
            return shell_open(&spec.program, &spec.args);
        }
        return spawn_win(spec);
    }
    #[cfg(not(windows))]
    {
        let _ = (spec.creation_flags, spec.open_with_shell, spec.console);
        let mut cmd = std::process::Command::new(&spec.program);
        cmd.args(&spec.args);
        // Null, not piped: a pipe would fill and stall the child, and waiting
        // on it is what froze the message loop.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(child) => {
                drop(child);
                Ok(())
            }
            Err(e) => Err(format!("{}: {e}", spec.program)),
        }
    }
}

#[cfg(windows)]
fn spawn_win(spec: &SpawnSpec) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
    };

    let app: Vec<u16> = OsStr::new(&spec.program).encode_wide().chain([0]).collect();
    let mut cmdline: Vec<u16> = OsStr::new(&win_command_line(&spec.program, &spec.args))
        .encode_wide()
        .chain([0])
        .collect();
    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // dwFlags stays 0: no STARTF_USESTDHANDLES, so the new console owns stdio.
    let mut pi = PROCESS_INFORMATION::default();
    let flags = PROCESS_CREATION_FLAGS(spec.creation_flags | CREATE_UNICODE_ENVIRONMENT);
    let created = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            PWSTR(cmdline.as_mut_ptr()),
            None,
            None,
            false,
            flags,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    };
    match created {
        Ok(()) => {
            unsafe {
                if !pi.hProcess.0.is_null() {
                    let _ = CloseHandle(pi.hProcess);
                }
                if !pi.hThread.0.is_null() {
                    let _ = CloseHandle(pi.hThread);
                }
            }
            Ok(())
        }
        Err(e) => Err(format!("{}: {e}", spec.program)),
    }
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    if let Ok(root) = std::env::var("SystemRoot") {
        dirs.push(PathBuf::from(&root).join("System32"));
        dirs.push(PathBuf::from(root));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

/// PATH, then `App Paths`. An explicit path (`C:\...`, `\\server\...`, `./x`)
/// is used as given.
fn resolve_program(
    token: &str,
    dirs: &[PathBuf],
    app_path: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let token = token.trim().trim_matches('"');
    if token.is_empty() {
        return None;
    }
    if is_explicit_path(token) {
        return first_runnable(&with_exts(PathBuf::from(token)));
    }
    for dir in dirs {
        if let Some(found) = first_runnable(&with_exts(dir.join(token))) {
            return Some(found);
        }
    }
    for name in app_path_names(token) {
        if let Some(found) = app_path(&name) {
            if is_runnable(&found) {
                return Some(found);
            }
        }
    }
    None
}

fn app_path_names(token: &str) -> Vec<String> {
    let file = Path::new(token)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| token.to_string());
    let mut names = vec![file.clone()];
    if !file.to_ascii_lowercase().ends_with(".exe") {
        names.push(format!("{file}.exe"));
    }
    names
}

#[cfg(windows)]
fn with_exts(path: PathBuf) -> Vec<PathBuf> {
    let mut out = vec![path.clone()];
    if path.extension().is_none() {
        for ext in pathext() {
            let mut s = path.as_os_str().to_os_string();
            s.push(ext);
            out.push(PathBuf::from(s));
        }
    }
    out
}

#[cfg(not(windows))]
fn with_exts(path: PathBuf) -> Vec<PathBuf> {
    vec![path]
}

#[cfg(windows)]
fn pathext() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.MSC".into());
    raw.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with('.'))
        .collect()
}

fn first_runnable(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| is_runnable(p)).cloned()
}

fn is_runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

fn is_explicit_path(token: &str) -> bool {
    token.contains('\\')
        || token.contains('/')
        || (token.len() >= 3
            && token.as_bytes()[0].is_ascii_alphabetic()
            && token.as_bytes()[1] == b':'
            && (token.as_bytes()[2] == b'\\' || token.as_bytes()[2] == b'/'))
}

#[cfg(windows)]
fn lookup_app_path(name: &str) -> Option<PathBuf> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let keys = [
        (
            HKEY_CURRENT_USER,
            format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{name}"),
        ),
        (
            HKEY_LOCAL_MACHINE,
            format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{name}"),
        ),
        (
            HKEY_LOCAL_MACHINE,
            format!(r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths\{name}"),
        ),
    ];
    for (root, sub) in keys {
        if let Some(raw) = reg_default_sz(root, &sub) {
            let path = PathBuf::from(expand_percent(strip_quotes(raw.trim())));
            if is_runnable(&path) {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn lookup_app_path(_name: &str) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn reg_default_sz(root: windows::Win32::System::Registry::HKEY, subkey: &str) -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };
    let sub: Vec<u16> = OsStr::new(subkey).encode_wide().chain([0]).collect();
    unsafe {
        let mut key = Default::default();
        if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut key) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        let got = RegGetValueW(
            key,
            PCWSTR::null(),
            PCWSTR::null(),
            flags,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        if got != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let n = (size as usize / 2).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    t.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(t)
}

fn expand_percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(rel) = b[i + 1..].iter().position(|&c| c == b'%') {
                let name = &s[i + 1..i + 1 + rel];
                if !name.is_empty() && !name.contains(['\\', '/']) {
                    if let Ok(val) = std::env::var(name) {
                        out.push_str(&val);
                        i += rel + 2;
                        continue;
                    }
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn image_kind(path: &Path) -> ImageKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    classify_image(&ext, read_pe_subsystem(path))
}

fn classify_image(ext: &str, subsystem: Option<u16>) -> ImageKind {
    if is_batch(ext) || ext == "com" {
        return ImageKind::Console;
    }
    match subsystem {
        Some(2) => ImageKind::Gui,
        Some(3) | Some(7) => ImageKind::Console,
        _ => ImageKind::Unknown,
    }
}

fn read_pe_subsystem(path: &Path) -> Option<u16> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    pe_subsystem(&buf)
}

fn pe_subsystem(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 0x40 || bytes[0] != b'M' || bytes[1] != b'Z' {
        return None;
    }
    let e_lfanew = u32::from_le_bytes(bytes[0x3C..0x40].try_into().ok()?) as usize;
    let opt = e_lfanew.checked_add(24)?;
    if bytes.len() < opt + 70 {
        return None;
    }
    if &bytes[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return None;
    }
    let magic = u16::from_le_bytes(bytes[opt..opt + 2].try_into().ok()?);
    if magic != 0x10B && magic != 0x20B {
        return None;
    }
    Some(u16::from_le_bytes(
        bytes[opt + 68..opt + 70].try_into().ok()?,
    ))
}

fn is_batch(ext: &str) -> bool {
    matches!(ext, "bat" | "cmd")
}

fn is_shell_link(ext: &str) -> bool {
    matches!(ext, "lnk" | "appref-ms")
}

fn is_cmd_builtin(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "echo"
            | "dir"
            | "cd"
            | "chdir"
            | "set"
            | "type"
            | "copy"
            | "del"
            | "erase"
            | "ren"
            | "rename"
            | "md"
            | "mkdir"
            | "rd"
            | "rmdir"
            | "cls"
            | "exit"
            | "start"
            | "call"
            | "rem"
            | "ver"
            | "vol"
            | "path"
            | "prompt"
            | "title"
            | "color"
            | "pushd"
            | "popd"
            | "assoc"
            | "ftype"
            | "mklink"
            | "for"
            | "if"
            | "goto"
            | "shift"
            | "pause"
            | "move"
            | "date"
            | "time"
    )
}

fn needs_shell(s: &str) -> bool {
    let mut quoted = false;
    for c in s.chars() {
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted && matches!(c, '|' | '&' | '<' | '>' | '^' | '%' | '(' | ')' | '\n') {
            return true;
        }
    }
    false
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn quote_win(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '"')) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut bs = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => bs += 1,
            '"' => {
                out.extend(std::iter::repeat('\\').take(bs * 2 + 1));
                out.push('"');
                bs = 0;
            }
            c => {
                out.extend(std::iter::repeat('\\').take(bs));
                bs = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat('\\').take(bs * 2));
    out.push('"');
    out
}

fn win_command_line(program: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(1 + args.len());
    parts.push(quote_win(program));
    for a in args {
        parts.push(quote_win(a));
    }
    parts.join(" ")
}

#[cfg_attr(not(windows), allow(dead_code))]
fn join_params(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_win(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_runas(file: &str, args: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let verb: Vec<u16> = OsStr::new("runas").encode_wide().chain([0]).collect();
        let file_w: Vec<u16> = OsStr::new(file).encode_wide().chain([0]).collect();
        let params = join_params(args);
        let params_w: Vec<u16> = OsStr::new(&params).encode_wide().chain([0]).collect();
        let r = unsafe {
            ShellExecuteW(
                HWND(std::ptr::null_mut()),
                PCWSTR(verb.as_ptr()),
                PCWSTR(file_w.as_ptr()),
                if args.is_empty() {
                    PCWSTR::null()
                } else {
                    PCWSTR(params_w.as_ptr())
                },
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if r.0 as usize <= 32 {
            return Err(format!("runas failed ({})", r.0 as usize));
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (file, args);
        Err("elevate is Windows-only".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_resolve(_: &str) -> Option<PathBuf> {
        None
    }

    fn kind_console(_: &Path) -> ImageKind {
        ImageKind::Console
    }

    #[test]
    fn open_in_shell_rejects_empty() {
        assert!(open_in_shell(Path::new("")).is_err());
    }

    #[test]
    fn perform_describes_url_without_requiring_browser_success_path() {
        let a = katana_core::classify_command("https://example.com", false);
        assert_eq!(a, ResolvedAction::OpenUrl("https://example.com".into()));
        let a = katana_core::classify_command("> not this", false);
        match a {
            ResolvedAction::Shell { admin, .. } => assert!(!admin),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn powershell_cmd_and_wt_open_a_console_without_no_window() {
        let ps = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        let cmd = PathBuf::from(r"C:\Windows\System32\cmd.exe");
        let wt = PathBuf::from(r"C:\Users\me\AppData\Local\Microsoft\WindowsApps\wt.exe");
        let spec = plan_shell_with(
            "powershell",
            &|t| t.eq_ignore_ascii_case("powershell").then(|| ps.clone()),
            &kind_console,
        )
        .unwrap();
        assert_eq!(spec.program, ps.display().to_string());
        assert!(spec.args.is_empty());
        assert!(spec.console);
        assert!(!spec.open_with_shell);
        assert_eq!(
            spec.creation_flags,
            CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP
        );
        assert_eq!(spec.creation_flags & CREATE_NO_WINDOW, 0);
        assert_eq!(spec.creation_flags & DETACHED_PROCESS, 0);

        let spec = plan_shell_with(
            "powershell -NoProfile",
            &|t| t.eq_ignore_ascii_case("powershell").then(|| ps.clone()),
            &kind_console,
        )
        .unwrap();
        assert_eq!(spec.args, vec!["-NoProfile".to_string()]);

        let spec = plan_shell_with(
            "cmd",
            &|t| t.eq_ignore_ascii_case("cmd").then(|| cmd.clone()),
            &kind_console,
        )
        .unwrap();
        assert_eq!(spec.program, cmd.display().to_string());
        assert_eq!(spec.creation_flags & CREATE_NEW_CONSOLE, CREATE_NEW_CONSOLE);

        let spec = plan_shell_with(
            "wt",
            &|t| t.eq_ignore_ascii_case("wt").then(|| wt.clone()),
            &|_| ImageKind::Unknown,
        )
        .unwrap();
        assert_eq!(spec.program, wt.display().to_string());
        assert_eq!(spec.creation_flags & CREATE_NEW_CONSOLE, CREATE_NEW_CONSOLE);
        assert_eq!(spec.creation_flags & DETACHED_PROCESS, 0);
        assert_eq!(spec.creation_flags & CREATE_NO_WINDOW, 0);
    }

    #[test]
    fn gui_program_is_detached_not_given_a_console() {
        let dir = std::env::temp_dir().join(format!(
            "katana-pe-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notepad = dir.join("notepad.exe");
        std::fs::write(&notepad, pe_with_subsystem(2)).unwrap();
        let spec = plan_shell_with(
            "notepad",
            &|t| t.eq_ignore_ascii_case("notepad").then(|| notepad.clone()),
            &image_kind,
        )
        .unwrap();
        assert_eq!(spec.program, notepad.display().to_string());
        assert_eq!(spec.creation_flags, DETACHED_PROCESS);
        assert!(!spec.console);
        assert_eq!(spec.creation_flags & CREATE_NEW_CONSOLE, 0);
        assert_eq!(spec.creation_flags & CREATE_NO_WINDOW, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_syntax_and_builtins_do_not_wait_inside_cmd_no_window() {
        let spec = plan_shell_with(
            "dir | more",
            &|_| panic!("pipe is not resolved"),
            &kind_console,
        )
        .unwrap();
        assert_eq!(spec.creation_flags & CREATE_NO_WINDOW, 0);
        if cfg!(windows) {
            assert!(spec.program.to_ascii_lowercase().ends_with("cmd.exe"));
            assert_eq!(spec.args[0], "/C");
            assert_eq!(spec.creation_flags & CREATE_NEW_CONSOLE, CREATE_NEW_CONSOLE);
        } else {
            assert_eq!(spec.program, "/bin/sh");
            assert_eq!(spec.args[0], "-c");
        }

        let spec = plan_shell_with(
            "echo hi",
            &|_| Some(PathBuf::from("/should-not-be-used")),
            &kind_console,
        )
        .unwrap();
        assert_ne!(spec.program, "/should-not-be-used");
        assert_eq!(spec.creation_flags & CREATE_NO_WINDOW, 0);
    }

    #[test]
    fn missing_command_is_an_error_not_a_spawn() {
        let err =
            plan_shell_with("katana-missing-cmd-zzzy", &no_resolve, &kind_console).unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("katana-missing-cmd-zzzy"), "{err}");
        assert!(plan_shell_with("   ", &no_resolve, &kind_console).is_err());
    }

    #[test]
    fn resolve_uses_path_then_app_paths() {
        let root = std::env::temp_dir().join(format!(
            "katana-resolve-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path_dir = root.join("path");
        let app_dir = root.join("app");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::create_dir_all(&app_dir).unwrap();
        let on_path = path_dir.join("tool");
        let on_app = app_dir.join("chrome.exe");
        std::fs::write(&on_path, b"path").unwrap();
        std::fs::write(&on_app, b"app").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [&on_path, &on_app] {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let found = resolve_program("tool", &[path_dir.clone()], &|name| {
            (name.eq_ignore_ascii_case("tool.exe") || name == "tool").then(|| on_app.clone())
        });
        assert_eq!(found, Some(on_path));

        let found = resolve_program("chrome", &[], &|name| {
            name.eq_ignore_ascii_case("chrome.exe")
                .then(|| on_app.clone())
        });
        assert_eq!(found, Some(on_app));
        assert!(resolve_program("nope", &[path_dir], &|_| None).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pe_header_splits_gui_console_and_unknown() {
        assert_eq!(pe_subsystem(&pe_with_subsystem(2)), Some(2));
        assert_eq!(pe_subsystem(&pe_with_subsystem(3)), Some(3));
        assert_eq!(pe_subsystem(b"MZ"), None);
        assert_eq!(classify_image("exe", Some(2)), ImageKind::Gui);
        assert_eq!(classify_image("exe", Some(3)), ImageKind::Console);
        assert_eq!(classify_image("exe", None), ImageKind::Unknown);
        assert_eq!(classify_image("bat", None), ImageKind::Console);
        assert!(is_shell_link("lnk"));
    }

    #[test]
    fn command_line_quotes_paths_with_spaces() {
        assert_eq!(quote_win("powershell"), "powershell");
        assert_eq!(quote_win(""), "\"\"");
        assert_eq!(
            win_command_line(r"C:\Program Files\pwsh.exe", &["-NoLogo".into()]),
            r#""C:\Program Files\pwsh.exe" -NoLogo"#
        );
        assert_eq!(
            expand_percent("%NO_SUCH_KATANA_VAR%"),
            "%NO_SUCH_KATANA_VAR%"
        );
        std::env::set_var("KATANA_EXPAND_TEST", "expanded");
        assert_eq!(expand_percent(r"%KATANA_EXPAND_TEST%\bin"), r"expanded\bin");
    }

    #[test]
    fn spawn_does_not_wait_or_pipe_stdio() {
        let root = std::env::temp_dir().join(format!(
            "katana-detach-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let token = root.file_name().unwrap().to_string_lossy().into_owned();
        let script = root.join("sleep-long");
        std::fs::write(&script, "#!/bin/sh\nsleep 8\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cmdline = script.display().to_string();
        let started = std::time::Instant::now();
        let result = spawn_shell(&cmdline, false);
        let elapsed = started.elapsed();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "spawn waited for the child: {elapsed:?}"
        );

        let mut pid = None;
        for _ in 0..40 {
            pid = child_pid(&token);
            if pid.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let pid = pid.expect("child was not running after spawn returned");
        let stdout = std::fs::read_link(format!("/proc/{pid}/fd/1")).unwrap_or_default();
        let shown = stdout.display().to_string();
        assert!(
            !shown.contains("pipe"),
            "stdio is piped ({shown}); a console app would stall the parent"
        );
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_spawn_returns_quickly() {
        let started = std::time::Instant::now();
        let err = spawn_shell("katana-missing-cmd-zzzy", false).unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(err.contains("not found"), "{err}");
        assert!(!err.contains("caller thread"), "{err}");
    }

    #[test]
    fn launching_a_program_does_not_wait() {
        let root = std::env::temp_dir().join(format!(
            "katana-gui-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("gui-sleep");
        std::fs::write(&script, "#!/bin/sh\nsleep 8\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let started = std::time::Instant::now();
        let result = perform(&ResolvedAction::Launch {
            path: script.display().to_string(),
            args: Vec::new(),
            admin: false,
        });
        assert!(result.is_ok(), "{result:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
        if let Some(pid) = child_pid("gui-sleep") {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    fn child_pid(token: &str) -> Option<u32> {
        let out = std::process::Command::new("ps")
            .args(["-eo", "pid,args"])
            .output()
            .ok()?;
        let self_pid = std::process::id();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let line = line.trim();
            if !line.contains(token) {
                continue;
            }
            let pid = line.split_whitespace().next()?.parse().ok()?;
            if pid != self_pid {
                return Some(pid);
            }
        }
        None
    }

    fn pe_with_subsystem(subsystem: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 0x100];
        buf[0] = b'M';
        buf[1] = b'Z';
        buf[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        buf[0x80..0x84].copy_from_slice(b"PE\0\0");
        let opt = 0x80 + 24;
        buf[opt] = 0x0B;
        buf[opt + 1] = 0x01;
        buf[opt + 68..opt + 70].copy_from_slice(&subsystem.to_le_bytes());
        buf
    }
}
