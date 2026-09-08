# Katana

Windows-native swiss knife. One process, one hotkey, no webview. Fast, small, many-talented.

## Budgets

- Release exe `< 8 MB` (LTO + strip + `opt-level = "z"`)
- Hotkey → focused input `< 16 ms` (HWND created once)
- Keystroke ranking `< 5 ms`
- Idle RSS lean; no extra threads except clipboard pump + USN watchers
- Never rebuild or walk the disk while holding the UI mutex
- Compile nucleo `Pattern` once per query; keep only top-k file hits

## Ban list (default graph)

Do not add: `tauri`, `wry`, `tokio`, `reqwest`, crate `image`, webview, egui/eframe/wgpu.

## Commands

```
cargo test --workspace
cargo build --release -p katana
# then overwrite the launched copy:
#   %USERPROFILE%\tools\katana.exe
katana                  # overlay
katana todo add "…"     # CLI (TODO_DB_PATH override)
```

Install path: always replace `%USERPROFILE%\tools\katana.exe` after a release build (user preference). If the exe is locked, stop the tray process and overwrite — no sidecar names.

## Layout

App crate is `crates/katana`. Shared logic is in `crates/katana-*`. Win32 (hotkey, tray, BitBlt, MFT) stays behind adapters. Tests drive shipped parse/rank/token/todo/index/clip/shot functions.
