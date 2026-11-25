fn main() {
    println!("cargo:rerun-if-changed=assets/katana.ico");
    println!("cargo:rerun-if-changed=assets/katana.rc");
    // Best-effort exe icon. Missing rc.exe is fine — tray/splash still use the ICO.
    let rc = std::path::Path::new("assets").join("katana.rc");
    if !rc.exists() {
        return;
    }
    let Some(rc_exe) = find_rc() else {
        return;
    };
    let out = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
    let res = std::path::Path::new(&out).join("katana.res");
    let status = std::process::Command::new(rc_exe)
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg(&rc)
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        return;
    }
    // MSVC link.exe takes the .res. GNU ld does not, so windres makes a COFF object.
    let link = if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        let Some(windres) = find_windres() else {
            return;
        };
        let obj = std::path::Path::new(&out).join("katana.o");
        let status = std::process::Command::new(windres)
            .args(["-O", "coff", "-i"])
            .arg(&res)
            .arg("-o")
            .arg(&obj)
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            return;
        }
        obj
    } else {
        res
    };
    println!("cargo:rustc-link-arg={}", link.display());
}

fn find_rc() -> Option<std::path::PathBuf> {
    if let Ok(p) = which("rc.exe") {
        return Some(p);
    }
    // Cross builds: llvm-rc accepts the same /nologo /fo arguments.
    if let Ok(p) = which("llvm-rc") {
        return Some(p);
    }
    if let Ok(p) = which("llvm-rc.exe") {
        return Some(p);
    }
    // Typical VS / Windows SDK locations.
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    for root in roots {
        if let Ok(rd) = std::fs::read_dir(root) {
            for ent in rd.flatten() {
                let cand = ent.path().join("x64").join("rc.exe");
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

fn find_windres() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("WINDRES") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(target) = std::env::var("TARGET") {
        if let Some(arch) = target.strip_suffix("-pc-windows-gnu") {
            if let Ok(p) = which(&format!("{arch}-w64-mingw32-windres")) {
                return Some(p);
            }
        }
    }
    which("windres").ok()
}

fn which(name: &str) -> Result<std::path::PathBuf, ()> {
    let paths = std::env::var("PATH").map_err(|_| ())?;
    for p in std::env::split_paths(&paths) {
        let cand = p.join(name);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(())
}
