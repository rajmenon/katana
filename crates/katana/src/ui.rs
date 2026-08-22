//! Tray-resident overlay: thin rounded prompt.

use std::sync::Mutex;

use crate::launcher::Launcher;

pub(crate) static STATE: Mutex<Option<Launcher>> = Mutex::new(None);

#[cfg(windows)]
mod win {
    use super::STATE;
    use crate::capture::{capture, set_clipboard_png_dib};
    use crate::launcher::{Launcher, Outcome};
    use katana_core::ShotMode;
    use katana_shot::{dib_from_bgra, png_from_bgra};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
    use std::sync::Mutex;

    use windows::core::w;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT,
        POINT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
        EndPaint, FillRect, InvalidateRect, LineTo, MoveToEx, SelectObject, SetBkMode,
        SetDIBitsToDevice, SetTextColor, SetWindowRgn, TextOutW, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, PS_SOLID, TRANSPARENT, PAINTSTRUCT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
        MOD_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadCursorW,
        PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
        SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage, CS_DROPSHADOW, CS_HREDRAW,
        CS_VREDRAW,
        HWND_TOPMOST, IDC_ARROW, MF_STRING, MSG, PM_REMOVE, SM_CXSCREEN,
        SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, TPM_BOTTOMALIGN,
        TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_ACTIVATE, WM_CHAR, WM_CLIPBOARDUPDATE, WM_COMMAND,
        WM_DESTROY, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONUP, WM_PAINT, WM_RBUTTONUP, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    const HOT_PALETTE: i32 = 1;
    const HOT_SHOT_REGION: i32 = 2;
    const HOT_SHOT_WIN: i32 = 3;
    const HOT_SHOT_SCREEN: i32 = 4;
    const HOT_SHOT_LAST: i32 = 5;
    const TRAY_CB: u32 = 0x8001;
    const WM_WAKE: u32 = 0x8002;
    const MENU_SHOW: usize = 1001;
    const MENU_QUIT: usize = 1002;
    const MENU_KW: usize = 1003;
    const MENU_TODO: usize = 1004;
    const MENU_SET: usize = 1005;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_RETURN: u16 = 0x0D;
    const VK_BACK: u16 = 0x08;
    const VK_DOWN: u16 = 0x28;
    const VK_UP: u16 = 0x26;
    const VK_DELETE: u16 = 0x2E;
    const VK_MENU: u16 = 0x12;
    const VK_V: u16 = 0x56;
    const WM_SYSKEYDOWN: u32 = 0x0104;

    const W: i32 = 640;
    const H_BAR: i32 = 56;
    const ROW_H: i32 = 40;
    const MAX_ROWS: i32 = 8;
    const RADIUS: i32 = 14;

    // BGR COLORREF — zinc / amber
    const BG: u32 = 0x0014_1416;
    const ROW_SEL: u32 = 0x0022_1C14;
    const TEXT: u32 = 0x00F4_F4F5;
    const MUTED: u32 = 0x00A1_A1AA;
    const ACCENT: u32 = 0x0024_A5F5;
    const BORDER: u32 = 0x002E_2A24;

    static VISIBLE: AtomicBool = AtomicBool::new(false);
    static LAST_H: AtomicI32 = AtomicI32::new(0);
    static FONT: AtomicIsize = AtomicIsize::new(0);
    static FONT_SM: AtomicIsize = AtomicIsize::new(0);
    static SHELL_ICONS: Mutex<Option<HashMap<String, isize>>> = Mutex::new(None);
    static SINGLETON: AtomicIsize = AtomicIsize::new(0);
    static LAST_CLIP_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match msg {
            m if m == WM_WAKE => {
                show(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if w.0 as u32 == 0 && VISIBLE.load(Ordering::SeqCst) {
                    hide(hwnd);
                }
                LRESULT(0)
            }
            WM_CHAR => {
                let ch = w.0 as u32;
                if (32..127).contains(&ch) && !alt_down() {
                    let c = char::from_u32(ch).unwrap_or('?');
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(ln) = g.as_mut() {
                            if ln.composing() {
                                ln.push_todo_char(c);
                            } else if crate::search::todo_list_mode(&ln.query) && c == '+' {
                                ln.begin_todo_add();
                            } else if crate::search::todo_list_mode(&ln.query) && c == '%' {
                                ln.begin_todo_progress();
                            } else {
                                let mut q = ln.query.clone();
                                q.push(c);
                                ln.set_query(&q);
                            }
                        }
                    }
                    relayout(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = w.0 as u16;
                if vk == VK_V && alt_down() {
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(ln) = g.as_mut() {
                            if crate::search::is_todo_blade(&ln.query) && !ln.composing() {
                                match ln.mark_selected_todo_done() {
                                    Ok(_) => {}
                                    Err(e) => tray_info(hwnd, "Katana", &e),
                                }
                            }
                        }
                    }
                    relayout(hwnd);
                    return LRESULT(0);
                }
                if msg == WM_SYSKEYDOWN {
                    return DefWindowProcW(hwnd, msg, w, l);
                }
                match vk {
                    VK_ESCAPE => {
                        let hide_overlay = if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if ln.composing() {
                                    ln.cancel_todo_input();
                                    false
                                } else if crate::search::is_blade_query(&ln.query) {
                                    ln.go_home();
                                    false
                                } else {
                                    true
                                }
                            } else {
                                true
                            }
                        } else {
                            true
                        };
                        if hide_overlay {
                            hide(hwnd);
                        } else {
                            relayout(hwnd);
                        }
                    }
                    VK_RETURN => {
                        let handled = if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if ln.composing() {
                                    if let Err(e) = ln.commit_todo_input() {
                                        tray_info(hwnd, "Katana", &e);
                                    }
                                    true
                                } else if crate::search::todo_list_mode(&ln.query) {
                                    ln.begin_todo_edit();
                                    true
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if handled {
                            relayout(hwnd);
                        } else if let Err(e) = execute_and_maybe_hide(hwnd) {
                            tray_info(hwnd, "Katana", &e);
                        }
                    }
                    VK_BACK => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if ln.composing() {
                                    ln.pop_todo_char();
                                } else {
                                    let mut q = ln.query.clone();
                                    q.pop();
                                    ln.set_query(&q);
                                }
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_DOWN => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if !ln.hits.is_empty() && !ln.composing() {
                                    ln.selected = (ln.selected + 1).min(ln.hits.len() - 1);
                                }
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_UP => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if !ln.composing() {
                                    ln.selected = ln.selected.saturating_sub(1);
                                }
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_DELETE => {
                        let is_clip = STATE
                            .lock()
                            .ok()
                            .and_then(|g| {
                                g.as_ref().map(|ln| crate::search::is_clip_blade(&ln.query))
                            })
                            .unwrap_or(false);
                        if is_clip {
                            if let Ok(mut g) = STATE.lock() {
                                if let Some(ln) = g.as_mut() {
                                    match ln.delete_selected_clip() {
                                        Ok(_) => {}
                                        Err(e) => tray_info(hwnd, "Katana", &e),
                                    }
                                }
                            }
                            relayout(hwnd);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                match w.0 as i32 {
                    HOT_PALETTE => toggle(hwnd),
                    HOT_SHOT_REGION => run_shot(hwnd, ShotMode::Region),
                    HOT_SHOT_WIN => run_shot(hwnd, ShotMode::Window),
                    HOT_SHOT_SCREEN => run_shot(hwnd, ShotMode::Screen),
                    HOT_SHOT_LAST => run_shot(hwnd, ShotMode::Last),
                    _ => {}
                }
                LRESULT(0)
            }
            m if m == TRAY_CB => {
                match l.0 as u32 {
                    x if x == WM_LBUTTONUP => toggle(hwnd),
                    x if x == WM_RBUTTONUP => tray_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                match w.0 & 0xFFFF {
                    MENU_SHOW => show(hwnd),
                    MENU_KW => crate::editors::open_studio("shortcuts"),
                    MENU_TODO => crate::editors::open_studio("todos"),
                    MENU_SET => crate::editors::open_studio("settings"),
                    MENU_QUIT => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            m if m == WM_CLIPBOARDUPDATE => {
                ingest_clipboard();
                LRESULT(0)
            }
            WM_DESTROY => {
                remove_tray(hwnd);
                let _ = UnregisterHotKey(hwnd, HOT_PALETTE);
                let h = SINGLETON.swap(0, Ordering::SeqCst);
                if h != 0 {
                    let _ = CloseHandle(HANDLE(h as *mut _));
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }

    const PREVIEW_H: i32 = 148;

    fn alt_down() -> bool {
        unsafe { GetKeyState(VK_MENU as i32) as u16 & 0x8000 != 0 }
    }

    fn overlay_height(hits: usize, file_mode: bool, preview: bool, footer: bool) -> i32 {
        let rows = (hits as i32).min(MAX_ROWS);
        if rows == 0 {
            H_BAR
        } else {
            H_BAR
                + 6
                + rows * ROW_H
                + if footer { 28 } else { 8 }
                + if file_mode { 16 } else { 0 }
                + if preview { PREVIEW_H } else { 0 }
        }
    }

    fn center_pos(h: i32) -> (i32, i32) {
        let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        ((sw - W) / 2, sh / 5 + (sh / 2 - h) / 8)
    }

    unsafe fn apply_round(hwnd: HWND, h: i32) {
        let rgn = CreateRoundRectRgn(0, 0, W + 1, h + 1, RADIUS * 2, RADIUS * 2);
        let _ = SetWindowRgn(hwnd, rgn, true);
        // Win11 rounded corners when DWM is present
        type DwmSet = unsafe extern "system" fn(HWND, u32, *const core::ffi::c_void, u32) -> i32;
        if let Ok(lib) = windows::Win32::System::LibraryLoader::LoadLibraryW(w!("dwmapi.dll")) {
            use windows::Win32::System::LibraryLoader::GetProcAddress;
            use windows::core::s;
            if let Some(f) = GetProcAddress(lib, s!("DwmSetWindowAttribute")) {
                let f: DwmSet = std::mem::transmute(f);
                let pref: u32 = 2; // DWMWCP_ROUND
                let _ = f(hwnd, 33, &pref as *const u32 as *const _, 4);
            }
        }
    }

    fn relayout(hwnd: HWND) {
        let (n, files, preview, footer) = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref().map(|l| {
                    let sel = l.hits.get(l.selected);
                    let preview = sel
                        .map(|h| {
                            h.kind == katana_core::HitKind::Clip && h.subtitle.contains("image")
                        })
                        .unwrap_or(false);
                    let footer = crate::search::blade_footer(&l.query, l.composing()).is_some();
                    (
                        l.hits.len(),
                        l.hits.iter().any(|h| h.kind == katana_core::HitKind::File),
                        preview,
                        footer,
                    )
                })
            })
            .unwrap_or((0, false, false, false));
        let h = overlay_height(n, files, preview, footer);
        if h == LAST_H.load(Ordering::Relaxed) && VISIBLE.load(Ordering::SeqCst) {
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            return;
        }
        LAST_H.store(h, Ordering::Relaxed);
        let (x, y) = center_pos(h);
        unsafe {
            apply_round(hwnd, h);
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                x,
                y,
                W,
                h,
                if VISIBLE.load(Ordering::SeqCst) {
                    SWP_SHOWWINDOW
                } else {
                    SWP_NOACTIVATE
                },
            );
            let _ = InvalidateRect(hwnd, None, false);
        }
    }

    fn ingest_clipboard() {
        unsafe {
            use windows::Win32::System::DataExchange::{
                CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
                RegisterClipboardFormatW,
            };
            use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
            use windows::core::w;
            const CF_UNICODETEXT: u32 = 13;
            const CF_TEXT: u32 = 1;
            const CF_DIB: u32 = 8;
            let seq = GetClipboardSequenceNumber();
            if seq == LAST_CLIP_SEQ.load(Ordering::Relaxed) && seq != 0 {
                return;
            }
            let mut opened = false;
            for _ in 0..12 {
                if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
            if !opened {
                return;
            }
            let png_fmt = RegisterClipboardFormatW(w!("PNG"));
            let png = if png_fmt != 0 {
                GetClipboardData(png_fmt).ok().and_then(|h| hglobal_bytes(h.0))
            } else {
                None
            };
            let dib_png = GetClipboardData(CF_DIB).ok().and_then(|h| hglobal_bytes(h.0)).and_then(
                |dib| {
                    katana_shot::bgra_from_dib(&dib)
                        .ok()
                        .and_then(|(w, h, bgra)| png_from_bgra(w, h, &bgra).ok())
                },
            );
            let text = GetClipboardData(CF_UNICODETEXT)
                .ok()
                .and_then(|h| {
                    let p = GlobalLock(windows::Win32::Foundation::HGLOBAL(h.0));
                    if p.is_null() {
                        return None;
                    }
                    let mut n = 0usize;
                    let ptr = p as *const u16;
                    while *ptr.add(n) != 0 && n < 1_000_000 {
                        n += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr, n);
                    let s = String::from_utf16(slice).ok();
                    let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(h.0));
                    s
                })
                .or_else(|| {
                    GetClipboardData(CF_TEXT).ok().and_then(|h| {
                        let bytes = hglobal_bytes(h.0)?;
                        let n = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                        Some(String::from_utf8_lossy(&bytes[..n]).into_owned())
                    })
                });
            let _ = CloseClipboard();
            let Some(payload) = crate::capture::pick_clip_payload(text, png, dib_png) else {
                return;
            };
            LAST_CLIP_SEQ.store(seq, Ordering::Relaxed);
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    match payload {
                        crate::capture::ClipPayload::Text(s) => {
                            let _ = ln.ingest_clipboard_text(&s);
                        }
                        crate::capture::ClipPayload::Image(b) => {
                            let _ = ln.ingest_clipboard_image(&b);
                        }
                    }
                }
            }

            fn hglobal_bytes(raw: *mut core::ffi::c_void) -> Option<Vec<u8>> {
                if raw.is_null() {
                    return None;
                }
                let hg = windows::Win32::Foundation::HGLOBAL(raw);
                let p = unsafe { GlobalLock(hg) };
                if p.is_null() {
                    return None;
                }
                let n = unsafe { GlobalSize(hg) };
                let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n) }.to_vec();
                let _ = unsafe { GlobalUnlock(hg) };
                if bytes.is_empty() {
                    None
                } else {
                    Some(bytes)
                }
            }
        }
    }

    fn run_shot(hwnd: HWND, mode: ShotMode) {
        hide(hwnd);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let last = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|l| l.last_region));
        let frame = match capture(mode, last) {
            Ok(f) => f,
            Err(e) => {
                if e != "cancelled" {
                    tray_info(hwnd, "Katana", &format!("Screenshot failed: {e}"));
                }
                return;
            }
        };
        if let Ok(mut g) = STATE.lock() {
            if let Some(ln) = g.as_mut() {
                let _ = ln.complete_capture(mode, frame.rect, &frame.bgra);
            }
        }
        if let (Ok(png), Ok(dib)) = (
            png_from_bgra(frame.rect.w as u32, frame.rect.h as u32, &frame.bgra),
            dib_from_bgra(frame.rect.w as u32, frame.rect.h as u32, &frame.bgra),
        ) {
            let _ = set_clipboard_png_dib(&png, &dib);
            let settings = crate::persist::Settings::load(&crate::persist::settings_path());
            if settings.save_shots {
                let path = crate::persist::next_shot_path();
                if std::fs::write(&path, &png).is_ok() {
                    tray_info(
                        hwnd,
                        "Screenshot saved",
                        &format!("{}\n(also on clipboard)", path.display()),
                    );
                    return;
                }
            }
            tray_info(
                hwnd,
                "Screenshot copied",
                "On the clipboard.\nSettings → save files to Pictures\\Katana",
            );
        }
    }

    fn tray_info(hwnd: HWND, title: &str, text: &str) {
        use windows::Win32::UI::Shell::{
            Shell_NotifyIconW, NIF_INFO, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW,
        };
        unsafe {
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_INFO;
            nid.dwInfoFlags = NIIF_INFO;
            let t: Vec<u16> = title.encode_utf16().chain([0]).collect();
            let b: Vec<u16> = text.encode_utf16().chain([0]).collect();
            let nt = t.len().min(nid.szInfoTitle.len());
            nid.szInfoTitle[..nt].copy_from_slice(&t[..nt]);
            let nb = b.len().min(nid.szInfo.len());
            nid.szInfo[..nb].copy_from_slice(&b[..nb]);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn execute_and_maybe_hide(hwnd: HWND) -> Result<(), String> {
        let out = {
            let mut g = STATE.lock().map_err(|e| e.to_string())?;
            let ln = g.as_mut().ok_or("no launcher")?;
            ln.execute_current()?
        };
        match out {
            Outcome::NeedCapture(mode) => run_shot(hwnd, mode),
            Outcome::OpenStudio(tab) => {
                hide(hwnd);
                crate::editors::open_studio(tab);
            }
            Outcome::Ran(_) | Outcome::Todo(_) | Outcome::Shot { .. } | Outcome::Copied(_) => {
                hide(hwnd);
            }
            Outcome::Listed(_) => relayout(hwnd),
        }
        Ok(())
    }

    unsafe fn blit_bgra(
        hdc: windows::Win32::Graphics::Gdi::HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        bgra: &[u8],
    ) {
        if w <= 0 || h <= 0 || bgra.is_empty() {
            return;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = SetDIBitsToDevice(
            hdc,
            x,
            y,
            w as u32,
            h as u32,
            0,
            0,
            0,
            h as u32,
            bgra.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
        );
    }

    fn todo_percent_from_subtitle(sub: &str) -> Option<i32> {
        katana_todo::percent_from_label(sub).map(|n| n as i32)
    }

    unsafe fn draw_todo_bar(
        hdc: windows::Win32::Graphics::Gdi::HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        pct: i32,
    ) {
        let track = CreateSolidBrush(COLORREF(0x002E_2A24));
        let fill = CreateSolidBrush(COLORREF(ACCENT));
        let track_rc = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        FillRect(hdc, &track_rc, track);
        let fw = ((w - 2) * pct / 100).max(if pct > 0 { 2 } else { 0 });
        if fw > 0 {
            let fill_rc = RECT {
                left: x + 1,
                top: y + 1,
                right: x + 1 + fw,
                bottom: y + h - 1,
            };
            FillRect(hdc, &fill_rc, fill);
        }
        let _ = DeleteObject(fill);
        let _ = DeleteObject(track);
    }

    unsafe fn draw_item_icon(
        hdc: windows::Win32::Graphics::Gdi::HDC,
        x: i32,
        y: i32,
        h: &katana_core::Hit,
    ) {
        if h.kind == katana_core::HitKind::File {
            if let Some(icon) = shell_icon(&h.subtitle, h.is_dir) {
                use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL};
                let _ = DrawIconEx(hdc, x, y, icon, 16, 16, 0, None, DI_NORMAL);
                return;
            }
        }
        let color = if h.is_dir || h.kind == katana_core::HitKind::File && h.is_dir {
            COLORREF(0x0038_B8E8)
        } else if h.kind == katana_core::HitKind::File {
            COLORREF(0x00D0_D0D4)
        } else {
            COLORREF(ACCENT)
        };
        let br = CreateSolidBrush(color);
        let r = RECT {
            left: x,
            top: y,
            right: x + 14,
            bottom: y + 12,
        };
        FillRect(hdc, &r, br);
        let _ = DeleteObject(br);
    }

    fn shell_icon(path: &str, is_dir: bool) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
        use windows::Win32::UI::Shell::{
            SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES,
        };
        let key = if is_dir {
            "dir".to_string()
        } else {
            std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
        };
        if let Ok(g) = SHELL_ICONS.lock() {
            if let Some(map) = g.as_ref() {
                if let Some(&h) = map.get(&key) {
                    return Some(windows::Win32::UI::WindowsAndMessaging::HICON(h as *mut _));
                }
            }
        }
        let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut info = SHFILEINFOW::default();
        let attr = if is_dir {
            FILE_ATTRIBUTE_DIRECTORY.0
        } else {
            FILE_ATTRIBUTE_NORMAL.0
        };
        let flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES;
        let n = unsafe {
            SHGetFileInfoW(
                windows::core::PCWSTR(w.as_ptr()),
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(attr),
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };
        if n == 0 || info.hIcon.is_invalid() {
            return None;
        }
        if let Ok(mut g) = SHELL_ICONS.lock() {
            g.get_or_insert_with(HashMap::new)
                .insert(key, info.hIcon.0 as isize);
        }
        Some(info.hIcon)
    }

    unsafe fn overlay_fonts() -> (
        windows::Win32::Graphics::Gdi::HFONT,
        windows::Win32::Graphics::Gdi::HFONT,
    ) {
        use windows::Win32::Graphics::Gdi::HFONT;
        let a = FONT.load(Ordering::Relaxed);
        let b = FONT_SM.load(Ordering::Relaxed);
        if a != 0 && b != 0 {
            return (HFONT(a as *mut _), HFONT(b as *mut _));
        }
        let font = CreateFontW(
            -16, 0, 0, 0, 500, 0, 0, 0, 1, 0, 0, 5, 0, w!("Segoe UI"),
        );
        let small = CreateFontW(
            -12, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, w!("Segoe UI"),
        );
        FONT.store(font.0 as isize, Ordering::Relaxed);
        FONT_SM.store(small.0 as isize, Ordering::Relaxed);
        (font, small)
    }

    unsafe fn paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);

        let bg = CreateSolidBrush(COLORREF(BG));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);

        let pen = CreatePen(PS_SOLID, 1, COLORREF(BORDER));
        let old_pen = SelectObject(hdc, pen);
        let hollow = windows::Win32::Graphics::Gdi::GetStockObject(
            windows::Win32::Graphics::Gdi::NULL_BRUSH,
        );
        let old_br = SelectObject(hdc, hollow);
        let _ = windows::Win32::Graphics::Gdi::RoundRect(
            hdc,
            0,
            0,
            rc.right,
            rc.bottom,
            RADIUS * 2,
            RADIUS * 2,
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_br);
        let _ = DeleteObject(pen);

        SetBkMode(hdc, TRANSPARENT);
        let (font, small) = overlay_fonts();
        let old_f = SelectObject(hdc, font);

        let (q, prompt, rows, sel, composing) = {
            let g = STATE.lock().ok();
            match g.as_ref().and_then(|x| x.as_ref()) {
                Some(ln) => (
                    ln.query.clone(),
                    ln.prompt_text(),
                    ln.hits.clone(),
                    ln.selected,
                    ln.composing(),
                ),
                None => (String::new(), String::new(), Vec::new(), 0, false),
            }
        };

        let empty = prompt.is_empty();
        SetTextColor(hdc, COLORREF(if empty { MUTED } else { TEXT }));
        let shown_prompt = if empty { "Search" } else { prompt.as_str() };
        let wide: Vec<u16> = shown_prompt.encode_utf16().collect();
        let _ = TextOutW(hdc, 20, 18, &wide);

        // caret bar
        let accent = CreateSolidBrush(COLORREF(ACCENT));
        let bar = RECT {
            left: 0,
            top: 14,
            right: 3,
            bottom: 42,
        };
        FillRect(hdc, &bar, accent);
        let _ = DeleteObject(accent);

        let file_mode = rows.iter().any(|h| h.kind == katana_core::HitKind::File);
        let mut y0 = H_BAR + 4;
        if file_mode {
            SelectObject(hdc, small);
            SetTextColor(hdc, COLORREF(MUTED));
            let hdr: Vec<u16> = "Name                              Type                 Size"
                .encode_utf16()
                .collect();
            let _ = TextOutW(hdc, 40, y0, &hdr);
            y0 += 16;
        }
        let shown = rows.len().min(MAX_ROWS as usize);
        for (i, h) in rows.iter().take(shown).enumerate() {
            let y = y0 + (i as i32) * ROW_H;
            if i == sel {
                let selb = CreateSolidBrush(COLORREF(ROW_SEL));
                let rr = RECT {
                    left: 8,
                    top: y,
                    right: rc.right - 8,
                    bottom: y + ROW_H - 2,
                };
                FillRect(hdc, &rr, selb);
                let _ = DeleteObject(selb);
                let mark = CreateSolidBrush(COLORREF(ACCENT));
                let mk = RECT {
                    left: 8,
                    top: y + 8,
                    right: 11,
                    bottom: y + ROW_H - 10,
                };
                FillRect(hdc, &mk, mark);
                let _ = DeleteObject(mark);
            }
            draw_item_icon(hdc, 16, y + 10, h);
            SetTextColor(hdc, COLORREF(TEXT));
            SelectObject(hdc, font);
            let title: Vec<u16> = h.title.encode_utf16().collect();
            let _ = TextOutW(hdc, 40, y + 4, &title);
            SetTextColor(hdc, COLORREF(MUTED));
            SelectObject(hdc, small);
            if h.kind == katana_core::HitKind::File {
                let typ = h.type_name.as_deref().unwrap_or(if h.is_dir {
                    "File folder"
                } else {
                    "File"
                });
                let sz = match h.size {
                    Some(n) => katana_index::format_size(n),
                    None if h.is_dir => String::new(),
                    None => "—".into(),
                };
                let typ_w: Vec<u16> = typ.encode_utf16().collect();
                let sz_w: Vec<u16> = sz.encode_utf16().collect();
                let _ = TextOutW(hdc, 280, y + 6, &typ_w);
                let _ = TextOutW(hdc, 460, y + 6, &sz_w);
                let path_txt = if i == sel {
                    crate::search::launch_path(h).unwrap_or_else(|| h.subtitle.clone())
                } else {
                    h.subtitle.clone()
                };
                let path_w: Vec<u16> = path_txt.encode_utf16().collect();
                let _ = TextOutW(hdc, 40, y + 22, &path_w);
            } else {
                let kind = format!("{:?}", h.kind).to_ascii_lowercase();
                let sub = format!("{kind}   {}", h.subtitle);
                let sw: Vec<u16> = sub.encode_utf16().collect();
                let _ = TextOutW(hdc, 40, y + 22, &sw);
                if h.kind == katana_core::HitKind::Todo {
                    if let Some(pct) = todo_percent_from_subtitle(&h.subtitle) {
                        draw_todo_bar(hdc, rc.right - 92, y + 12, 68, 8, pct);
                    }
                }
            }
        }

        if shown > 0 {
            SelectObject(hdc, small);
            SetTextColor(hdc, COLORREF(MUTED));
            let foot_y = H_BAR + 6 + shown as i32 * ROW_H + 6 + if file_mode { 16 } else { 0 };
            if let Some(foot) = crate::search::blade_footer(&q, composing) {
                let foot: Vec<u16> = foot.encode_utf16().collect();
                let _ = TextOutW(hdc, 20, foot_y, &foot);
            }
            if let Some(h) = rows.get(sel) {
                if h.kind == katana_core::HitKind::Clip && h.subtitle.contains("image") {
                    if let Some(id) = h.id.strip_prefix("clip:").and_then(|s| s.parse::<i64>().ok())
                    {
                        let preview = STATE.lock().ok().and_then(|mut g| {
                            g.as_mut().and_then(|ln| ln.clip_preview(id))
                        });
                        if let Some((tw, th, bgra)) = preview {
                            let x = 20;
                            let y = foot_y + 22;
                            let frame = CreatePen(PS_SOLID, 1, COLORREF(BORDER));
                            let old_p = SelectObject(hdc, frame);
                            let _ = MoveToEx(hdc, x - 1, y - 1, None);
                            let _ = LineTo(hdc, x + tw as i32 + 1, y - 1);
                            let _ = LineTo(hdc, x + tw as i32 + 1, y + th as i32 + 1);
                            let _ = LineTo(hdc, x - 1, y + th as i32 + 1);
                            let _ = LineTo(hdc, x - 1, y - 1);
                            SelectObject(hdc, old_p);
                            let _ = DeleteObject(frame);
                            blit_bgra(hdc, x, y, tw as i32, th as i32, &bgra);
                        }
                    }
                }
            }
        }

        SelectObject(hdc, old_f);
        let _ = EndPaint(hwnd, &ps);
    }

    fn ensure_icon_file() -> std::path::PathBuf {
        let ico = include_bytes!("../assets/katana.ico");
        let dir = crate::persist::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("katana.ico");
        let stale = match std::fs::read(&path) {
            Ok(b) => b.as_slice() != ico.as_slice(),
            Err(_) => true,
        };
        if stale {
            let _ = std::fs::write(&path, ico);
        }
        path
    }

    pub(super) fn load_app_icon_size(cx: i32, cy: i32) -> windows::Win32::UI::WindowsAndMessaging::HICON {
        use windows::Win32::UI::WindowsAndMessaging::{
            LoadIconW, LoadImageW, IMAGE_ICON, LR_LOADFROMFILE, IDI_APPLICATION,
        };
        let path = ensure_icon_file();
        let w: Vec<u16> = path.to_string_lossy().encode_utf16().chain([0]).collect();
        unsafe {
            if let Ok(h) = LoadImageW(
                None,
                windows::core::PCWSTR(w.as_ptr()),
                IMAGE_ICON,
                cx,
                cy,
                LR_LOADFROMFILE,
            ) {
                return windows::Win32::UI::WindowsAndMessaging::HICON(h.0);
            }
            LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
        }
    }

    fn load_app_icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
        load_app_icon_size(16, 16)
    }

    unsafe fn add_tray(hwnd: HWND) {
        use windows::Win32::UI::Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
        };
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
        nid.uCallbackMessage = TRAY_CB;
        nid.hIcon = load_app_icon();
        let tip: Vec<u16> = "Katana — Alt+Space\0".encode_utf16().collect();
        let n = tip.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }

    unsafe fn remove_tray(hwnd: HWND) {
        use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }

    fn tray_menu(hwnd: HWND) {
        unsafe {
            let menu = CreatePopupMenu().unwrap_or_default();
            let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show Katana\tAlt+Space"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_KW, w!("Edit shortcuts…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_TODO, w!("Edit todos…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SET, w!("Settings…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
        }
    }

    fn lower_thread_priority() {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
            };
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }

    fn hide(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        VISIBLE.store(false, Ordering::SeqCst);
        LAST_H.store(0, Ordering::Relaxed);
    }

    fn show(hwnd: HWND) {
        ingest_clipboard();
        if let Ok(mut g) = STATE.lock() {
            if let Some(ln) = g.as_mut() {
                ln.set_query("");
            }
        }
        VISIBLE.store(true, Ordering::SeqCst);
        relayout(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    unsafe fn toggle(hwnd: HWND) {
        if VISIBLE.load(Ordering::SeqCst) {
            hide(hwnd);
        } else {
            show(hwnd);
        }
    }

    fn claim_singleton() -> bool {
        use windows::Win32::System::Threading::CreateMutexW;
        unsafe {
            let Ok(handle) = CreateMutexW(None, true, w!("Local\\KatanaSingleton")) else {
                return true;
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                if let Ok(hwnd) = FindWindowW(w!("KatanaOverlay"), None) {
                    let _ = PostMessageW(hwnd, WM_WAKE, WPARAM(0), LPARAM(0));
                }
                false
            } else {
                SINGLETON.store(handle.0 as isize, Ordering::SeqCst);
                true
            }
        }
    }

    pub fn run_overlay(smoke: bool) -> Result<(), String> {
        if !smoke && !claim_singleton() {
            return Ok(());
        }
        let splash = if smoke {
            None
        } else {
            crate::splash::Splash::show()
        };
        let launcher = if smoke {
            let dir = std::env::temp_dir().join("katana-smoke");
            let _ = std::fs::create_dir_all(&dir);
            Launcher::open(
                &dir.join("t.db"),
                &dir.join("c.db"),
                katana_index::NameIndex::new(),
                true,
            )?
        } else {
            Launcher::open_default(false)?
        };
        *STATE.lock().map_err(|e| e.to_string())? = Some(launcher);
        if let Some(s) = splash {
            s.finish();
        }

        unsafe {
            let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = w!("KatanaOverlay");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: load_app_icon_size(32, 32),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let (x, y) = center_pos(H_BAR);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class,
                w!("Katana"),
                WS_POPUP,
                x,
                y,
                W,
                H_BAR,
                None,
                None,
                hinst,
                None,
            )
            .map_err(|e| e.to_string())?;

            use windows::Win32::UI::WindowsAndMessaging::{
                SetLayeredWindowAttributes, LWA_ALPHA,
            };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 245, LWA_ALPHA);
            apply_round(hwnd, H_BAR);
            let _ = ShowWindow(hwnd, SW_HIDE);
            add_tray(hwnd);

            if !smoke {
                std::thread::Builder::new()
                    .name("katana-index".into())
                    .spawn(|| {
                        lower_thread_priority();
                        let (files, apps, bookmarks) = crate::launcher::build_indexes();
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.engine.files = files;
                                ln.engine.apps = apps;
                                ln.engine.bookmarks = bookmarks;
                            }
                        }
                    })
                    .ok();
            }

            use windows::Win32::System::DataExchange::AddClipboardFormatListener;
            let _ = AddClipboardFormatListener(hwnd);

            let _ = RegisterHotKey(hwnd, HOT_PALETTE, MOD_ALT, 0x20);
            let _ = RegisterHotKey(hwnd, HOT_SHOT_REGION, HOT_KEY_MODIFIERS(0), 0x2C);
            let _ = RegisterHotKey(hwnd, HOT_SHOT_WIN, MOD_ALT, 0x2C);
            let _ = RegisterHotKey(hwnd, HOT_SHOT_SCREEN, MOD_CONTROL, 0x2C);
            let _ = RegisterHotKey(hwnd, HOT_SHOT_LAST, MOD_SHIFT, 0x2C);

            if !smoke {
                std::thread::Builder::new()
                    .name("katana-usn".into())
                    .spawn(|| {
                        lower_thread_priority();
                        if let Some(letter) = std::env::var("SystemDrive")
                            .ok()
                            .and_then(|s| s.chars().next())
                        {
                            let mut cur = katana_index::open_usn_cursor(letter).unwrap_or(
                                katana_index::UsnCursor {
                                    journal_id: 0,
                                    next_usn: 0,
                                },
                            );
                            loop {
                                let rebuild = {
                                    let mut g = match STATE.lock() {
                                        Ok(g) => g,
                                        Err(_) => break,
                                    };
                                    match g.as_mut() {
                                        Some(ln) => match katana_index::poll_usn(
                                            letter,
                                            &mut cur,
                                            &mut ln.engine.files,
                                        ) {
                                            Ok(_) => false,
                                            Err(katana_index::IndexError::NeedsRebuild) => true,
                                            Err(_) => false,
                                        },
                                        None => false,
                                    }
                                };
                                if rebuild {
                                    let files = crate::launcher::rebuild_file_index();
                                    if let Ok(mut g) = STATE.lock() {
                                        if let Some(ln) = g.as_mut() {
                                            ln.engine.files = files;
                                        }
                                    }
                                    if let Ok(c) = katana_index::open_usn_cursor(letter) {
                                        cur = c;
                                    }
                                }
                                std::thread::sleep(std::time::Duration::from_millis(750));
                            }
                        }
                    })
                    .ok();
            }

            if smoke {
                let mut msg = MSG::default();
                let _ = PeekMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0, PM_REMOVE);
                if let Ok(mut g) = STATE.lock() {
                    if let Some(ln) = g.as_mut() {
                        let _ = ln.set_query("");
                        let _ = ln.set_query("g rust");
                        let _ = ln.execute_query("g rust");
                    }
                }
                let _ = DestroyWindow(hwnd);
                return Ok(());
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).into() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
pub fn run(smoke: bool) -> Result<(), String> {
    win::run_overlay(smoke)
}

#[cfg(windows)]
pub(crate) fn load_app_icon_size(cx: i32, cy: i32) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    win::load_app_icon_size(cx, cy)
}

#[cfg(not(windows))]
pub fn run(_smoke: bool) -> Result<(), String> {
    Err("Katana GUI is Windows-only".into())
}
