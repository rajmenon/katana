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
    if matches!(status, Ok(s) if s.success()) {
        println!("cargo:rustc-link-arg={}", res.display());
    }
}

fn find_rc() -> Option<std::path::PathBuf> {
    if let Ok(p) = which("rc.exe") {
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
