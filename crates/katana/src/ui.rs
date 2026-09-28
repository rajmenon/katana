//! Tray-resident overlay: thin rounded prompt.

use std::sync::Mutex;

use crate::launcher::Launcher;

pub(crate) static STATE: Mutex<Option<Launcher>> = Mutex::new(None);
static LAST_SHOT: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Tray callback `lParam` (low word). Balloon click opens the last screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayEvent {
    Toggle,
    Menu,
    OpenLastShot,
}

pub(crate) fn classify_tray_event(lparam: u32) -> Option<TrayEvent> {
    const NIN_SELECT: u32 = 0x0400;
    const NIN_KEYSELECT: u32 = 0x0401;
    const NIN_BALLOONUSERCLICK: u32 = 0x0405;
    const WM_CONTEXTMENU: u32 = 0x007B;
    const LBUTTONUP: u32 = 0x0202;
    const RBUTTONUP: u32 = 0x0205;
    match lparam & 0xFFFF {
        x if x == LBUTTONUP || x == NIN_SELECT || x == NIN_KEYSELECT => Some(TrayEvent::Toggle),
        x if x == RBUTTONUP || x == WM_CONTEXTMENU => Some(TrayEvent::Menu),
        x if x == NIN_BALLOONUSERCLICK => Some(TrayEvent::OpenLastShot),
        _ => None,
    }
}

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
        DrawTextW, IntersectClipRect, RestoreDC, SaveDC, SetDIBitsToDevice, SetTextColor,
        SetWindowRgn, TextOutW, DT_CALCRECT, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
        DT_VCENTER,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, PS_SOLID, TRANSPARENT,
        PAINTSTRUCT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
        MOD_SHIFT, MOD_WIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadCursorW,
        PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
        SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage, CS_DROPSHADOW, CS_HREDRAW,
        CS_VREDRAW,
        HWND_TOPMOST, IDC_ARROW, MF_CHECKED, MF_STRING, MSG, PM_REMOVE, SM_CXSCREEN,
        SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, TPM_BOTTOMALIGN,
        TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_ACTIVATE, WM_CHAR, WM_CLIPBOARDUPDATE, WM_COMMAND,
        KillTimer, SetTimer, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEWHEEL,
        WM_PAINT, WM_TIMER, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    const HOT_PALETTE: i32 = 1;
    const HOT_SHOT_REGION: i32 = 2;
    const HOT_SHOT_WIN: i32 = 3;
    const HOT_SHOT_SCREEN: i32 = 4;
    const HOT_SHOT_LAST: i32 = 5;
    const HOT_CLIP: i32 = 6;
    const HOT_FILES: i32 = 7;
    const TRAY_CB: u32 = 0x8001;
    const WM_WAKE: u32 = 0x8002;
    const MENU_SHOW: usize = 1001;
    const MENU_QUIT: usize = 1002;
    const MENU_KW: usize = 1003;
    const MENU_TODO: usize = 1004;
    const MENU_SET: usize = 1005;
    const MENU_SHOTS: usize = 1006;
    const MENU_AWAKE: usize = 1007;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_RETURN: u16 = 0x0D;
    const VK_BACK: u16 = 0x08;
    const VK_DOWN: u16 = 0x28;
    const VK_UP: u16 = 0x26;
    const VK_DELETE: u16 = 0x2E;
    const VK_LEFT: u16 = 0x25;
    const VK_RIGHT: u16 = 0x27;
    const VK_HOME: u16 = 0x24;
    const VK_END: u16 = 0x23;
    const VK_PRIOR: u16 = 0x21;
    const VK_NEXT: u16 = 0x22;
    const SCROLLBAR_W: i32 = 12;
    const VK_MENU: u16 = 0x12;
    const VK_V: u16 = 0x56;
    const WM_SYSKEYDOWN: u32 = 0x0104;

    const PALETTE_W: i32 = 640;
    const FILE_W: i32 = 780;
    const H_BAR: i32 = 56;
    const ROW_H: i32 = 40;
    const FILE_ROW_H: i32 = 28;
    const MAX_ROWS: i32 = 8;
    const RADIUS: i32 = 14;

    // BGR COLORREF — zinc / amber
    const BG: u32 = 0x0014_1416;
    const ROW_SEL: u32 = 0x0022_1C14;
    const TEXT: u32 = 0x00F4_F4F5;
    const MUTED: u32 = 0x00A1_A1AA;
    const ACCENT: u32 = 0x0024_A5F5;
    const BORDER: u32 = 0x002E_2A24;
    const FAIL: u32 = 0x0028_28F0; // BGR red flash
    const TIMER_FAIL: usize = 1;

    static VISIBLE: AtomicBool = AtomicBool::new(false);
    static LAST_H: AtomicI32 = AtomicI32::new(0);
    static LAST_W: AtomicI32 = AtomicI32::new(0);
    static LAST_TARGET: AtomicIsize = AtomicIsize::new(0);
    static LAST_FOCUS: AtomicIsize = AtomicIsize::new(0);
    static ANCHOR_X: AtomicI32 = AtomicI32::new(0);
    static ANCHOR_Y: AtomicI32 = AtomicI32::new(0);
    static PROMPT_SCROLL: AtomicI32 = AtomicI32::new(0);
    static FONT: AtomicIsize = AtomicIsize::new(0);
    static FONT_SM: AtomicIsize = AtomicIsize::new(0);
    static SHELL_ICONS: Mutex<Option<HashMap<String, isize>>> = Mutex::new(None);
    static SINGLETON: AtomicIsize = AtomicIsize::new(0);
    static LAST_CLIP_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static FAIL_FLASH: AtomicI32 = AtomicI32::new(0);

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wndproc_inner(hwnd, msg, w, l)))
        {
            Ok(r) => r,
            Err(_) => {
                if msg == WM_DESTROY {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
        }
    }

    unsafe fn wndproc_inner(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match msg {
            m if m == WM_WAKE => {
                show(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_TIMER => {
                if w.0 == TIMER_FAIL {
                    let n = FAIL_FLASH.fetch_sub(1, Ordering::SeqCst) - 1;
                    if n <= 0 {
                        FAIL_FLASH.store(0, Ordering::SeqCst);
                        let _ = KillTimer(hwnd, TIMER_FAIL);
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                }
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
                                ln.insert_at_caret(c);
                            } else if crate::search::todo_list_mode(&ln.query) && c == '+' {
                                ln.begin_todo_add();
                            } else if crate::search::todo_list_mode(&ln.query) && c == '%' {
                                ln.begin_todo_progress();
                            } else {
                                ln.insert_at_caret(c);
                            }
                        }
                    }
                    relayout(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = w.0 as u16;
                if vk == 0x43 && ctrl_down() && !alt_down() {
                    let copied = STATE.lock().ok().and_then(|g| {
                        g.as_ref().and_then(|ln| {
                            if ln.composing() {
                                None
                            } else {
                                ln.copy_selected_path().ok()
                            }
                        })
                    });
                    if let Some(path) = copied {
                        tray_info(hwnd, "Path copied", &path);
                        return LRESULT(0);
                    }
                }
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
                                ln.go_back()
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
                                ln.backspace_at_caret();
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_LEFT => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.move_caret(-1);
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_RIGHT => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.move_caret(1);
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_HOME => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.caret_home();
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_END => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.caret_end();
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_PRIOR => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if crate::search::is_file_blade(&ln.query) && !ln.composing() {
                                    ln.page_files(false);
                                }
                            }
                        }
                        relayout(hwnd);
                    }
                    VK_NEXT => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                if crate::search::is_file_blade(&ln.query) && !ln.composing() {
                                    ln.page_files(true);
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
                                    ln.ensure_file_scroll();
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
                                    ln.ensure_file_scroll();
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
                        } else if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.delete_at_caret();
                            }
                            relayout(hwnd);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = ((l.0 as u32) & 0xFFFF) as i16 as i32;
                let y = (((l.0 as u32) >> 16) & 0xFFFF) as i16 as i32;
                let header_top = H_BAR + 4;
                let in_header = y >= header_top && y < header_top + 20;
                let file_mode = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|ln| crate::search::is_file_blade(&ln.query)))
                    .unwrap_or(false);
                if y < H_BAR {
                    set_caret_from_click(hwnd, x);
                    relayout(hwnd);
                } else if file_mode && in_header {
                    let col = crate::search::file_column_at(x);
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(ln) = g.as_mut() {
                            ln.cycle_file_sort(col);
                        }
                    }
                    relayout(hwnd);
                } else if file_mode && x >= FILE_W - SCROLLBAR_W - 4 {
                    let list_top = header_top + 20;
                    let track_h = (crate::search::FILE_PAGE as i32) * FILE_ROW_H;
                    let click_y = y - list_top;
                    if click_y >= 0 && click_y <= track_h {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(ln) = g.as_mut() {
                                ln.file_scroll = crate::search::scroll_from_track_click(
                                    ln.hits.len(),
                                    crate::search::FILE_PAGE,
                                    track_h,
                                    click_y,
                                );
                                ln.ensure_file_scroll();
                            }
                        }
                        relayout(hwnd);
                    }
                } else if file_mode && y >= header_top + 20 {
                    let list_top = header_top + 20;
                    let row = ((y - list_top) / FILE_ROW_H) as usize;
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(ln) = g.as_mut() {
                            let abs = ln.file_scroll + row;
                            if abs < ln.hits.len() {
                                ln.selected = abs;
                            }
                        }
                    }
                    relayout(hwnd);
                }
                LRESULT(0)
            }
            m if m == WM_MOUSEWHEEL => {
                let delta = ((w.0 as u32 >> 16) as i16) as i32;
                let steps = -delta / 120;
                if steps != 0 {
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(ln) = g.as_mut() {
                            if crate::search::is_file_blade(&ln.query) {
                                ln.scroll_files_by(steps);
                            }
                        }
                    }
                    relayout(hwnd);
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                match w.0 as i32 {
                    HOT_PALETTE => toggle(hwnd),
                    HOT_CLIP => show_blade(hwnd, "/clip"),
                    HOT_FILES => show_blade(hwnd, "/f "),
                    HOT_SHOT_REGION => run_shot(hwnd, ShotMode::Region),
                    HOT_SHOT_WIN => run_shot(hwnd, ShotMode::Window),
                    HOT_SHOT_SCREEN => run_shot(hwnd, ShotMode::Screen),
                    HOT_SHOT_LAST => run_shot(hwnd, ShotMode::Last),
                    _ => {}
                }
                LRESULT(0)
            }
            m if m == TRAY_CB => {
                match crate::ui::classify_tray_event(l.0 as u32) {
                    Some(crate::ui::TrayEvent::Toggle) => toggle(hwnd),
                    Some(crate::ui::TrayEvent::Menu) => tray_menu(hwnd),
                    Some(crate::ui::TrayEvent::OpenLastShot) => open_last_shot(),
                    None => {}
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                match w.0 & 0xFFFF {
                    MENU_SHOW => show(hwnd),
                    MENU_KW => crate::editors::open_studio("shortcuts"),
                    MENU_TODO => crate::editors::open_studio("todos"),
                    MENU_SET => crate::editors::open_studio("settings"),
                    MENU_SHOTS => open_shot_folder(),
                    MENU_AWAKE => toggle_awake_ui(hwnd),
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
                let _ = UnregisterHotKey(hwnd, HOT_CLIP);
                let _ = UnregisterHotKey(hwnd, HOT_FILES);
                let h = SINGLETON.swap(0, Ordering::SeqCst);
                if h != 0 {
                    let _ = CloseHandle(HANDLE(h as *mut _));
                }
                crate::keepawake::set(false);
                let _ = KillTimer(hwnd, TIMER_FAIL);
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

    fn ctrl_down() -> bool {
        unsafe { GetKeyState(0x11) as u16 & 0x8000 != 0 }
    }

    fn overlay_height(hits: usize, file_mode: bool, preview: bool, footer: bool) -> i32 {
        let rows = (hits as i32).min(MAX_ROWS);
        if rows == 0 {
            H_BAR
        } else {
            let row_h = if file_mode { FILE_ROW_H } else { ROW_H };
            H_BAR
                + 6
                + rows * row_h
                + if footer {
                    if file_mode {
                        40
                    } else {
                        28
                    }
                } else {
                    8
                }
                + if file_mode { 22 } else { 0 }
                + if preview { PREVIEW_H } else { 0 }
        }
    }

    fn overlay_width(file_mode: bool) -> i32 {
        if file_mode { FILE_W } else { PALETTE_W }
    }

    fn work_area_at(x: i32, y: i32) -> RECT {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };
        unsafe {
            let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                return mi.rcWork;
            }
            RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            }
        }
    }

    fn place_near_cursor(width: i32, h: i32) -> (i32, i32) {
        let ax = ANCHOR_X.load(Ordering::Relaxed);
        let ay = ANCHOR_Y.load(Ordering::Relaxed);
        let gap = 16;
        let work = work_area_at(ax, ay);
        let mut x = ax + gap;
        let mut y = ay + gap;
        if x + width > work.right {
            x = ax - width - gap;
        }
        if y + h > work.bottom {
            y = ay - h - gap;
        }
        x = x.max(work.left).min(work.right.saturating_sub(width));
        y = y.max(work.top).min(work.bottom.saturating_sub(h));
        (x, y)
    }

    unsafe fn apply_round(hwnd: HWND, width: i32, h: i32) {
        let rgn = CreateRoundRectRgn(0, 0, width + 1, h + 1, RADIUS * 2, RADIUS * 2);
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
                        .map(crate::search::clip_hit_is_image)
                        .unwrap_or(false);
                    let footer = crate::search::blade_footer(&l.query, l.composing()).is_some();
                    (
                        l.hits.len(),
                        crate::search::is_file_blade(&l.query),
                        preview,
                        footer,
                    )
                })
            })
            .unwrap_or((0, false, false, false));
        let h = overlay_height(n, files, preview, footer);
        let width = overlay_width(files);
        if h == LAST_H.load(Ordering::Relaxed)
            && width == LAST_W.load(Ordering::Relaxed)
            && VISIBLE.load(Ordering::SeqCst)
        {
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            return;
        }
        LAST_H.store(h, Ordering::Relaxed);
        LAST_W.store(width, Ordering::Relaxed);
        let (x, y) = place_near_cursor(width, h);
        unsafe {
            apply_round(hwnd, width, h);
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                x,
                y,
                width,
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
            let dib = GetClipboardData(CF_DIB)
                .ok()
                .and_then(|h| hglobal_bytes(h.0));
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
            let Some(payload) = crate::capture::pick_clip_payload(text, png, dib) else {
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

    fn remember_foreground(overlay: HWND) {
        unsafe {
            use windows::Win32::Graphics::Gdi::ClientToScreen;
            use windows::Win32::UI::WindowsAndMessaging::{
                GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
            };
            let fg = GetForegroundWindow();
            if fg.0.is_null() || fg == overlay {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                ANCHOR_X.store(pt.x, Ordering::SeqCst);
                ANCHOR_Y.store(pt.y, Ordering::SeqCst);
                return;
            }
            LAST_TARGET.store(fg.0 as isize, Ordering::SeqCst);
            LAST_FOCUS.store(fg.0 as isize, Ordering::SeqCst);
            let tid = GetWindowThreadProcessId(fg, None);
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let mut anchored = false;
            if GetGUIThreadInfo(tid, &mut info).is_ok() {
                if !info.hwndFocus.0.is_null() {
                    LAST_FOCUS.store(info.hwndFocus.0 as isize, Ordering::SeqCst);
                }
                if !info.hwndCaret.0.is_null() {
                    let mut pt = POINT {
                        x: info.rcCaret.left,
                        y: info.rcCaret.bottom,
                    };
                    if ClientToScreen(info.hwndCaret, &mut pt).as_bool() {
                        ANCHOR_X.store(pt.x, Ordering::SeqCst);
                        ANCHOR_Y.store(pt.y, Ordering::SeqCst);
                        anchored = true;
                    }
                }
            }
            if !anchored {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                ANCHOR_X.store(pt.x, Ordering::SeqCst);
                ANCHOR_Y.store(pt.y, Ordering::SeqCst);
            }
        }
    }

    fn restore_target() {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_RESTORE};
            let h = LAST_TARGET.load(Ordering::SeqCst);
            if h != 0 {
                let target = HWND(h as *mut _);
                let _ = ShowWindow(target, SW_RESTORE);
                let _ = SetForegroundWindow(target);
            }
        }
    }

    fn run_shot(hwnd: HWND, mode: ShotMode) {
        hide(hwnd);
        restore_target();
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
            let path = crate::persist::next_shot_path();
            let saved = std::fs::write(&path, &png).is_ok();
            if saved {
                if let Ok(mut g) = super::LAST_SHOT.lock() {
                    *g = Some(path.clone());
                }
                tray_info(
                    hwnd,
                    "Screenshot saved",
                    &format!("{}\nClick to open  ·  also on clipboard", path.display()),
                );
            } else {
                tray_info(
                    hwnd,
                    "Screenshot copied",
                    "On the clipboard.\nCould not write Pictures\\Katana",
                );
            }
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
            Outcome::Paste => {
                hide(hwnd);
                let target = HWND(LAST_TARGET.load(Ordering::SeqCst) as *mut _);
                let focus = HWND(LAST_FOCUS.load(Ordering::SeqCst) as *mut _);
                if let Err(e) = crate::capture::paste_into_hwnd(target, focus) {
                    tray_info(hwnd, "Katana", &e);
                }
            }
            Outcome::Ran(_) | Outcome::Todo(_) | Outcome::Shot { .. } | Outcome::Copied(_) => {
                hide(hwnd);
            }
            Outcome::Listed(_) => {
                refresh_tray_tip(hwnd);
                relayout(hwnd);
            }
            Outcome::CmdFailed { exit, cmdline } => {
                blink_fail(hwnd);
                tray_info(
                    hwnd,
                    "Command failed",
                    &crate::exec::cmd_failed_label(exit, &cmdline),
                );
                relayout(hwnd);
            }
        }
        Ok(())
    }

    fn blink_fail(hwnd: HWND) {
        FAIL_FLASH.store(6, Ordering::SeqCst);
        unsafe {
            let _ = SetTimer(hwnd, TIMER_FAIL, 90, None);
            let _ = InvalidateRect(hwnd, None, false);
        }
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

    unsafe fn text_px(hdc: windows::Win32::Graphics::Gdi::HDC, s: &str) -> i32 {
        if s.is_empty() {
            return 0;
        }
        let mut w: Vec<u16> = s.encode_utf16().collect();
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let _ = DrawTextW(hdc, &mut w, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        (r.right - r.left).max(0)
    }

    unsafe fn caret_index_at_x(hdc: windows::Win32::Graphics::Gdi::HDC, prompt: &str, x: i32) -> usize {
        let scroll = PROMPT_SCROLL.load(Ordering::Relaxed);
        let rel = (x - 20 + scroll).max(0);
        let chars: Vec<char> = prompt.chars().collect();
        let mut best = 0usize;
        for i in 0..=chars.len() {
            let prefix: String = chars[..i].iter().collect();
            if text_px(hdc, &prefix) <= rel {
                best = i;
            } else {
                break;
            }
        }
        best
    }

    fn set_caret_from_click(hwnd: HWND, x: i32) {
        unsafe {
            use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
            let hdc = GetDC(hwnd);
            let (font, _) = overlay_fonts();
            let old = SelectObject(hdc, font);
            let prompt = STATE
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|ln| ln.prompt_text()))
                .unwrap_or_default();
            let idx = caret_index_at_x(hdc, &prompt, x);
            SelectObject(hdc, old);
            ReleaseDC(hwnd, hdc);
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    ln.set_prompt_caret(idx);
                }
            }
        }
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

        let fail = FAIL_FLASH.load(Ordering::SeqCst);
        let bg_color = if fail > 0 && fail % 2 == 1 { FAIL } else { BG };
        let bg = CreateSolidBrush(COLORREF(bg_color));
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

        let (q, prompt, rows, sel, composing, file_sort, file_desc, file_scroll, caret_i) = {
            let g = STATE.lock().ok();
            match g.as_ref().and_then(|x| x.as_ref()) {
                Some(ln) => (
                    ln.query.clone(),
                    ln.prompt_text(),
                    ln.hits.clone(),
                    ln.selected,
                    ln.composing(),
                    ln.file_sort,
                    ln.file_sort_desc,
                    ln.file_scroll,
                    ln.prompt_caret_index(),
                ),
                None => (
                    String::new(),
                    String::new(),
                    Vec::new(),
                    0,
                    false,
                    crate::search::FileSort::Score,
                    false,
                    0,
                    0,
                ),
            }
        };

        let empty = prompt.is_empty();
        SetTextColor(hdc, COLORREF(if empty { MUTED } else { TEXT }));
        let shown_prompt = if empty { "Search" } else { prompt.as_str() };
        let pad_l = 20;
        let pad_r = 16;
        let avail = (rc.right - pad_l - pad_r).max(8);
        let prefix: String = shown_prompt.chars().take(if empty { 0 } else { caret_i }).collect();
        let caret_px = text_px(hdc, &prefix);
        let scroll = crate::search::prompt_scroll_px(caret_px, avail);
        PROMPT_SCROLL.store(scroll, Ordering::Relaxed);
        let x0 = pad_l - scroll;
        let saved = SaveDC(hdc);
        let _ = IntersectClipRect(hdc, pad_l, 8, rc.right - pad_r, H_BAR);
        let wide: Vec<u16> = shown_prompt.encode_utf16().collect();
        let _ = TextOutW(hdc, x0, 18, &wide);
        if !empty {
            let cx = (x0 + caret_px).clamp(pad_l, rc.right - pad_r - 2);
            let caret_br = CreateSolidBrush(COLORREF(ACCENT));
            let caret_rc = RECT {
                left: cx,
                top: 16,
                right: cx + 2,
                bottom: 40,
            };
            FillRect(hdc, &caret_rc, caret_br);
            let _ = DeleteObject(caret_br);
        }
        let _ = RestoreDC(hdc, saved);

        // accent strip
        let accent = CreateSolidBrush(COLORREF(ACCENT));
        let bar = RECT {
            left: 0,
            top: 14,
            right: 3,
            bottom: 42,
        };
        FillRect(hdc, &bar, accent);
        let _ = DeleteObject(accent);

        let file_mode = crate::search::is_file_blade(&q);
        let row_h = if file_mode { FILE_ROW_H } else { ROW_H };
        let mut y0 = H_BAR + 4;
        if file_mode {
            SelectObject(hdc, small);
            let mark = |col: crate::search::FileSort, label: &str| -> String {
                if file_sort != col {
                    return label.to_string();
                }
                format!("{} {}", label, if file_desc { "▼" } else { "▲" })
            };
            let draw_hdr = |hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, col: crate::search::FileSort, label: &str| {
                let active = file_sort == col;
                SetTextColor(hdc, COLORREF(if active { TEXT } else { MUTED }));
                let w: Vec<u16> = mark(col, label).encode_utf16().collect();
                let _ = TextOutW(hdc, x, y0, &w);
            };
            draw_hdr(hdc, 40, crate::search::FileSort::Name, "Name");
            draw_hdr(hdc, 340, crate::search::FileSort::Modified, "Date modified");
            draw_hdr(hdc, 500, crate::search::FileSort::Type, "Type");
            draw_hdr(hdc, 650, crate::search::FileSort::Size, "Size");
            let line = CreatePen(PS_SOLID, 1, COLORREF(BORDER));
            let old_p = SelectObject(hdc, line);
            let _ = MoveToEx(hdc, 12, y0 + 16, None);
            let _ = LineTo(hdc, rc.right - 12, y0 + 16);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(line);
            y0 += 20;
        }
        let list_top = y0;
        let start = if file_mode {
            file_scroll.min(rows.len())
        } else {
            0
        };
        let shown = rows.len().saturating_sub(start).min(MAX_ROWS as usize);
        for (i, h) in rows.iter().skip(start).take(shown).enumerate() {
            let abs = start + i;
            let y = y0 + (i as i32) * row_h;
            let awake_on = h.id == "cmd:/awake" && crate::keepawake::is_on();
            if abs == sel || awake_on {
                let fill = if awake_on { ACCENT } else { ROW_SEL };
                let selb = CreateSolidBrush(COLORREF(fill));
                let rr = RECT {
                    left: 8,
                    top: y,
                    right: rc.right - 8,
                    bottom: y + row_h - 2,
                };
                FillRect(hdc, &rr, selb);
                let _ = DeleteObject(selb);
                let mark = CreateSolidBrush(COLORREF(if awake_on { TEXT } else { ACCENT }));
                let mk = RECT {
                    left: 8,
                    top: y + 6,
                    right: 11,
                    bottom: y + row_h - 8,
                };
                FillRect(hdc, &mk, mark);
                let _ = DeleteObject(mark);
            }
            draw_item_icon(hdc, 16, y + if file_mode { 6 } else { 10 }, h);
            SetTextColor(hdc, COLORREF(TEXT));
            SelectObject(hdc, if file_mode { small } else { font });
            let mut title: Vec<u16> = h.title.encode_utf16().collect();
            if file_mode {
                // Keep the name out of the date / type / size columns.
                let mut tr = RECT {
                    left: 40,
                    top: y + 2,
                    right: 332,
                    bottom: y + row_h - 2,
                };
                let _ = DrawTextW(
                    hdc,
                    &mut title,
                    &mut tr,
                    DT_END_ELLIPSIS | DT_SINGLELINE | DT_NOPREFIX | DT_VCENTER,
                );
            } else {
                let _ = TextOutW(hdc, 40, y + 4, &title);
            }
            SetTextColor(hdc, COLORREF(MUTED));
            SelectObject(hdc, small);
            if file_mode && h.kind == katana_core::HitKind::File {
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
                let date = h
                    .modified
                    .map(crate::search::format_modified)
                    .unwrap_or_default();
                let date_w: Vec<u16> = date.encode_utf16().collect();
                let typ_w: Vec<u16> = typ.encode_utf16().collect();
                let sz_w: Vec<u16> = sz.encode_utf16().collect();
                let _ = TextOutW(hdc, 340, y + 6, &date_w);
                let _ = TextOutW(hdc, 500, y + 6, &typ_w);
                let _ = TextOutW(hdc, 650, y + 6, &sz_w);
            } else if !file_mode {
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
                    let sub = format!("{typ}   {sz}   {}", h.subtitle);
                    let sw: Vec<u16> = sub.encode_utf16().collect();
                    let _ = TextOutW(hdc, 40, y + 22, &sw);
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
        }

        if file_mode {
            let track_h = (crate::search::FILE_PAGE as i32) * FILE_ROW_H;
            if let Some((ty, th)) = crate::search::scrollbar_thumb(
                rows.len(),
                crate::search::FILE_PAGE,
                file_scroll,
                track_h,
            ) {
                let bx = rc.right - SCROLLBAR_W - 6;
                let track = CreateSolidBrush(COLORREF(BORDER));
                let tr = RECT {
                    left: bx,
                    top: list_top,
                    right: bx + SCROLLBAR_W,
                    bottom: list_top + track_h,
                };
                FillRect(hdc, &tr, track);
                let _ = DeleteObject(track);
                let thumb = CreateSolidBrush(COLORREF(ACCENT));
                let tm = RECT {
                    left: bx + 2,
                    top: list_top + ty,
                    right: bx + SCROLLBAR_W - 2,
                    bottom: list_top + ty + th,
                };
                FillRect(hdc, &tm, thumb);
                let _ = DeleteObject(thumb);
            }
        }

        if shown > 0 {
            SelectObject(hdc, small);
            SetTextColor(hdc, COLORREF(MUTED));
            let foot_y = H_BAR
                + 6
                + shown as i32 * row_h
                + 6
                + if file_mode { 20 } else { 0 };
            if let Some(foot) = crate::search::blade_footer(&q, composing) {
                let foot: Vec<u16> = foot.encode_utf16().collect();
                let _ = TextOutW(hdc, 20, foot_y, &foot);
            }
            if file_mode {
                if let Some(h) = rows.get(sel) {
                    let path_txt =
                        crate::search::launch_path(h).unwrap_or_else(|| h.subtitle.clone());
                    let path_w: Vec<u16> = path_txt.encode_utf16().collect();
                    let _ = TextOutW(hdc, 20, foot_y + 14, &path_w);
                }
            }
            if let Some(h) = rows.get(sel) {
                if crate::search::clip_hit_is_image(h) {
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
        let tip: Vec<u16> = "Katana — Alt+Space · Win+Alt+C clip · Win+Alt+Space files\0"
            .encode_utf16()
            .collect();
        let n = tip.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        refresh_tray_tip(hwnd);
        nid.Anonymous.uVersion = 4; // NOTIFYICON_VERSION_4 — balloon click messages
        let _ = Shell_NotifyIconW(
            windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE(4), // NIM_SETVERSION
            &nid,
        );
    }

    unsafe fn remove_tray(hwnd: HWND) {
        use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }

    fn open_last_shot() {
        let path = super::LAST_SHOT.lock().ok().and_then(|g| g.clone());
        if let Some(p) = path {
            if let Err(e) = crate::exec::open_in_shell(&p) {
                // fall back to the folder if the file was moved
                let _ = e;
                open_shot_folder();
            }
        } else {
            open_shot_folder();
        }
    }

    fn open_shot_folder() {
        let dir = crate::persist::shot_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = crate::exec::open_in_shell(&dir);
    }

    fn refresh_tray_tip(hwnd: HWND) {
        use windows::Win32::UI::Shell::{
            Shell_NotifyIconW, NIF_TIP, NIM_MODIFY, NOTIFYICONDATAW,
        };
        unsafe {
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_TIP;
            let tip = if crate::keepawake::is_on() {
                "Katana — screen awake\0"
            } else {
                "Katana — Alt+Space · Win+Alt+C clip · Win+Alt+Space files\0"
            };
            let t: Vec<u16> = tip.encode_utf16().collect();
            let n = t.len().min(nid.szTip.len());
            nid.szTip[..n].copy_from_slice(&t[..n]);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn toggle_awake_ui(hwnd: HWND) {
        crate::keepawake::toggle();
        if let Ok(mut g) = STATE.lock() {
            if let Some(ln) = g.as_mut() {
                if ln.query.is_empty()
                    || matches!(ln.engine.route_str(&ln.query), katana_core::Route::KeepAwake)
                {
                    let q = ln.query.clone();
                    ln.set_query(&q);
                }
            }
        }
        refresh_tray_tip(hwnd);
        relayout(hwnd);
    }

    fn tray_menu(hwnd: HWND) {
        unsafe {
            let menu = CreatePopupMenu().unwrap_or_default();
            let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show Katana\tAlt+Space"));
            let awake_flags = if crate::keepawake::is_on() {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let awake_label = if crate::keepawake::is_on() {
                w!("Keep screen awake  ● ON")
            } else {
                w!("Keep screen awake")
            };
            let _ = AppendMenuW(menu, awake_flags, MENU_AWAKE, awake_label);
            let _ = AppendMenuW(menu, MF_STRING, MENU_KW, w!("Edit shortcuts…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_TODO, w!("Edit todos…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SET, w!("Settings…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SHOTS, w!("Open screenshots folder"));
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
        LAST_W.store(0, Ordering::Relaxed);
    }

    fn show_blade(hwnd: HWND, query: &str) {
        remember_foreground(hwnd);
        ingest_clipboard();
        if let Ok(mut g) = STATE.lock() {
            if let Some(ln) = g.as_mut() {
                ln.set_query(query);
            }
        }
        VISIBLE.store(true, Ordering::SeqCst);
        relayout(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    fn show(hwnd: HWND) {
        show_blade(hwnd, "");
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

            let (x, y) = place_near_cursor(PALETTE_W, H_BAR);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class,
                w!("Katana"),
                WS_POPUP,
                x,
                y,
                PALETTE_W,
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
            apply_round(hwnd, PALETTE_W, H_BAR);
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

            let settings = crate::persist::Settings::load(&crate::persist::settings_path());
            let reg = |id: i32, spec: &str, fallback_mod: HOT_KEY_MODIFIERS, fallback_vk: u32| {
                if let Some((mods, vk)) = crate::persist::parse_hotkey(spec) {
                    let _ = RegisterHotKey(hwnd, id, HOT_KEY_MODIFIERS(mods), vk);
                } else {
                    let _ = RegisterHotKey(hwnd, id, fallback_mod, fallback_vk);
                }
            };
            reg(HOT_PALETTE, &settings.hotkey, MOD_ALT, 0x20);
            reg(
                HOT_CLIP,
                &settings.clip_hotkey,
                MOD_WIN | MOD_ALT,
                b'C' as u32,
            );
            reg(
                HOT_FILES,
                &settings.files_hotkey,
                MOD_WIN | MOD_ALT,
                0x20,
            );
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
                                        Some(ln) => {
                                            match std::panic::catch_unwind(
                                                std::panic::AssertUnwindSafe(|| {
                                                    katana_index::poll_usn(
                                                        letter,
                                                        &mut cur,
                                                        &mut ln.engine.files,
                                                    )
                                                }),
                                            ) {
                                                Ok(Ok(_)) => false,
                                                Ok(Err(katana_index::IndexError::NeedsRebuild)) => {
                                                    true
                                                }
                                                _ => false,
                                            }
                                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_balloon_click_opens_last_shot() {
        assert_eq!(classify_tray_event(0x0202), Some(TrayEvent::Toggle));
        assert_eq!(classify_tray_event(0x0400), Some(TrayEvent::Toggle));
        assert_eq!(classify_tray_event(0x0205), Some(TrayEvent::Menu));
        assert_eq!(classify_tray_event(0x007B), Some(TrayEvent::Menu));
        assert_eq!(classify_tray_event(0x0405), Some(TrayEvent::OpenLastShot));
        assert_eq!(classify_tray_event(0x0405 | (1 << 16)), Some(TrayEvent::OpenLastShot));
        assert_eq!(classify_tray_event(0), None);
    }
}
