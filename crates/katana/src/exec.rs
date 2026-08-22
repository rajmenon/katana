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
            shell(cmdline, *admin)?;
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

fn shell(cmdline: &str, admin: bool) -> Result<(), String> {
    if admin {
        return shell_runas("powershell", &["-NoProfile".into(), "-Command".into(), cmdline.into()]);
    }
    let shell = if which("pwsh") {
        "pwsh"
    } else {
        "powershell"
    };
    let mut cmd = std::process::Command::new(shell);
    cmd.args(["-NoProfile", "-Command", cmdline]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
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

fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in path.split(';') {
        let p = std::path::Path::new(dir).join(format!("{name}.exe"));
        if p.exists() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
