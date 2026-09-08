use katana_core::ResolvedAction;

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
            run_shell(cmdline, *admin).map_err(|c| format!("exit {c}"))?;
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
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = url;
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
    let params = args.join(" ");
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

/// Run a command line and wait. `Err(exit_code)` when the process exits non-zero.
pub fn run_shell(cmdline: &str, admin: bool) -> Result<(), i32> {
    if admin {
        return shell_runas("powershell", &["-NoProfile".into(), "-Command".into(), cmdline.into()])
            .map_err(|_| 1);
    }
    // cmd /C so the process exit code is the command's (PowerShell often returns 0).
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", cmdline]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    match cmd.status() {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err(st.code().unwrap_or(1)),
        Err(_) => Err(1),
    }
}

pub fn cmd_failed_label(exit: i32, cmdline: &str) -> String {
    format!("exit {exit}: {cmdline}")
}

fn shell_runas(file: &str, args: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        // ShellExecuteEx "runas" — only on explicit elevate.
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let verb: Vec<u16> = OsStr::new("runas").encode_wide().chain([0]).collect();
        let file_w: Vec<u16> = OsStr::new(file).encode_wide().chain([0]).collect();
        let params = args.join(" ");
        let params_w: Vec<u16> = OsStr::new(&params).encode_wide().chain([0]).collect();
        let r = unsafe {
            ShellExecuteW(
                HWND(std::ptr::null_mut()),
                PCWSTR(verb.as_ptr()),
                PCWSTR(file_w.as_ptr()),
                PCWSTR(params_w.as_ptr()),
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

    #[test]
    fn open_in_shell_rejects_empty() {
        assert!(open_in_shell(std::path::Path::new("")).is_err());
    }

    #[test]
    fn run_shell_exit_1() {
        assert_eq!(run_shell("exit 1", false), Err(1));
        assert_eq!(run_shell("exit 0", false), Ok(()));
    }

    #[test]
    fn cmd_failed_label_includes_exit() {
        let s = cmd_failed_label(1, "false");
        assert!(s.starts_with("exit 1"), "{s}");
        assert!(s.contains("false"), "{s}");
    }

    #[test]
    fn perform_describes_url_without_requiring_browser_success_path() {
        // classify only — perform would spawn; we assert the action shape
        let a = katana_core::classify_command("https://example.com", false);
        assert_eq!(a, ResolvedAction::OpenUrl("https://example.com".into()));
        let a = katana_core::classify_command("> not this", false);
        // not a path, not url → shell
        match a {
            ResolvedAction::Shell { admin, .. } => assert!(!admin),
            other => panic!("{other:?}"),
        }
    }
}
