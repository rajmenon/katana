//! Win32 BitBlt / PrintWindow capture → BGRA, then PNG/DIB via katana-shot.

use katana_core::ShotMode;
use katana_shot::PhysRect;

#[derive(Debug, Clone)]
pub struct Frame {
    pub rect: PhysRect,
    pub bgra: Vec<u8>,
}

/// Capture according to mode. Hides `overlay` first so Katana is not in the shot.
#[cfg(windows)]
pub fn capture(mode: ShotMode, last: Option<PhysRect>) -> Result<Frame, String> {
    match mode {
        ShotMode::Region | ShotMode::Delay { .. } => select_and_capture_region(),
        ShotMode::Window => capture_foreground_window(),
        ShotMode::Screen => capture_virtual_screen(),
        ShotMode::Browser => capture_browser_or_window(),
        ShotMode::Last => {
            let r = last.ok_or_else(|| "no last region".to_string())?;
            capture_rect(r)
        }
    }
}

#[cfg(not(windows))]
pub fn capture(_mode: ShotMode, _last: Option<PhysRect>) -> Result<Frame, String> {
    Err("capture is Windows-only".into())
}

#[cfg(not(windows))]
pub fn capture_browser_or_window() -> Result<Frame, String> {
    Err("capture is Windows-only".into())
}

#[cfg(windows)]
pub fn capture_rect(r: PhysRect) -> Result<Frame, String> {
    if r.w <= 0 || r.h <= 0 {
        return Err("empty rect".into());
    }
    unsafe { bitblt_rect(r) }
}

#[cfg(windows)]
pub fn capture_virtual_screen() -> Result<Frame, String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        bitblt_rect(PhysRect { x, y, w, h, dpi: 96 })
    }
}

#[cfg(windows)]
pub fn capture_foreground_window() -> Result<Frame, String> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Err("no foreground window".into());
        }
        let mut rc = RECT::default();
        GetWindowRect(hwnd, &mut rc).map_err(|e| e.to_string())?;
        let r = PhysRect {
            x: rc.left,
            y: rc.top,
            w: rc.right - rc.left,
            h: rc.bottom - rc.top,
            dpi: 96,
        };
        bitblt_rect(r)
    }
}

#[cfg(windows)]
pub fn capture_browser_or_window() -> Result<Frame, String> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Err("no foreground window".into());
        }
        if is_browser_hwnd(hwnd) {
            capture_scrolling_window(hwnd).or_else(|_| capture_window_hwnd(hwnd))
        } else {
            capture_window_hwnd(hwnd)
        }
    }
}

#[cfg(windows)]
fn window_class_and_title(hwnd: windows::Win32::Foundation::HWND) -> (String, String) {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowTextW};
    unsafe {
        let mut cls = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut cls);
        let class = String::from_utf16_lossy(&cls[..n as usize]);
        let mut title = [0u16; 512];
        let t = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..t as usize]);
        (class, title)
    }
}

#[cfg(windows)]
pub fn is_browser_hwnd(hwnd: windows::Win32::Foundation::HWND) -> bool {
    let (class, title) = window_class_and_title(hwnd);
    let c = class.to_ascii_lowercase();
    let t = title.to_ascii_lowercase();
    c.contains("chrome_widgetwin")
        || c.contains("mozillawindowclass")
        || c.contains("operawindow")
        || c.contains("vivaldi")
        || ((c.contains("applicationframewindow") || c.contains("cabinetwclass"))
            && (t.contains("edge") || t.contains("chrome") || t.contains("firefox")))
}

#[cfg(windows)]
pub fn capture_window_hwnd(hwnd: windows::Win32::Foundation::HWND) -> Result<Frame, String> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        ReleaseDC, SelectObject, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut rc = RECT::default();
        GetWindowRect(hwnd, &mut rc).map_err(|e| e.to_string())?;
        let r = PhysRect {
            x: rc.left,
            y: rc.top,
            w: rc.right - rc.left,
            h: rc.bottom - rc.top,
            dpi: 96,
        };
        if r.w <= 0 || r.h <= 0 {
            return Err("empty window".into());
        }
        let hdc_scr = GetDC(hwnd);
        if hdc_scr.0.is_null() {
            return Err("GetDC failed".into());
        }
        let hdc_mem = CreateCompatibleDC(hdc_scr);
        let hbmp = CreateCompatibleBitmap(hdc_scr, r.w, r.h);
        let old = SelectObject(hdc_mem, hbmp);
        type PrintWindowFn = unsafe extern "system" fn(
            windows::Win32::Foundation::HWND,
            windows::Win32::Graphics::Gdi::HDC,
            u32,
        ) -> i32;
        let mut printed = false;
        if let Ok(lib) = windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!(
            "user32.dll"
        )) {
            if let Some(f) =
                windows::Win32::System::LibraryLoader::GetProcAddress(lib, windows::core::s!("PrintWindow"))
            {
                let f: PrintWindowFn = std::mem::transmute(f);
                printed = f(hwnd, hdc_mem, 2) != 0; // PW_RENDERFULLCONTENT
            }
        }
        if !printed {
            let desk = GetDC(windows::Win32::Foundation::HWND(std::ptr::null_mut()));
            let _ = BitBlt(hdc_mem, 0, 0, r.w, r.h, desk, r.x, r.y, SRCCOPY);
            ReleaseDC(windows::Win32::Foundation::HWND(std::ptr::null_mut()), desk);
        }
        let bgra = read_dib_section(hdc_mem, hbmp, r.w, r.h)?;
        SelectObject(hdc_mem, old);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_scr);
        Ok(Frame { rect: r, bgra })
    }
}

#[cfg(windows)]
fn client_phys(hwnd: windows::Win32::Foundation::HWND) -> Result<PhysRect, String> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(hwnd, &mut rc).map_err(|e| e.to_string())?;
        let mut pt = POINT {
            x: rc.left,
            y: rc.top,
        };
        let _ = ClientToScreen(hwnd, &mut pt);
        Ok(PhysRect {
            x: pt.x,
            y: pt.y,
            w: rc.right - rc.left,
            h: rc.bottom - rc.top,
            dpi: 96,
        })
    }
}

#[cfg(windows)]
fn hash_tail(bgra: &[u8], w: i32, h: i32, rows: i32) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let rows = rows.min(h).max(1) as usize;
    let w = w.max(1) as usize;
    let h = h.max(1) as usize;
    let stride = w * 4;
    let start = h.saturating_sub(rows) * stride;
    let mut hasher = DefaultHasher::new();
    bgra.get(start..).unwrap_or(bgra).hash(&mut hasher);
    hasher.finish()
}

#[cfg(windows)]
fn send_wheel(delta: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };
    unsafe {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: delta as u32,
                    dwFlags: MOUSEEVENTF_WHEEL,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(windows)]
fn capture_scrolling_window(hwnd: windows::Win32::Foundation::HWND) -> Result<Frame, String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SW_RESTORE, WM_VSCROLL,
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
        // SB_TOP = 6
        let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            WM_VSCROLL,
            windows::Win32::Foundation::WPARAM(6),
            windows::Win32::Foundation::LPARAM(0),
        );
        std::thread::sleep(std::time::Duration::from_millis(80));
        let r = client_phys(hwnd)?;
        if r.w < 8 || r.h < 8 {
            return Err("client too small".into());
        }
        let mut slices: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut last_hash = 0u64;
        let mut same = 0u32;
        for i in 0..12 {
            let frame = capture_rect(r)?;
            let hsh = hash_tail(&frame.bgra, r.w, r.h, 20);
            if i > 0 && hsh == last_hash {
                same += 1;
                if same >= 2 {
                    break;
                }
            } else {
                same = 0;
            }
            last_hash = hsh;
            slices.push((frame.rect.h as u32, frame.bgra));
            send_wheel(-360);
            std::thread::sleep(std::time::Duration::from_millis(90));
        }
        if slices.is_empty() {
            return Err("no slices".into());
        }
        let refs: Vec<(u32, &[u8])> = slices.iter().map(|(h, b)| (*h, b.as_slice())).collect();
        let (w, h, bgra) = katana_shot::stitch_vertical_bgra(r.w as u32, &refs)?;
        Ok(Frame {
            rect: PhysRect {
                x: r.x,
                y: r.y,
                w: w as i32,
                h: h as i32,
                dpi: 96,
            },
            bgra,
        })
    }
}

/// Restore the previous window + focused control, then insert clipboard (WM_PASTE and Ctrl+V).
#[cfg(windows)]
pub fn paste_into_hwnd(
    window: windows::Win32::Foundation::HWND,
    focus: windows::Win32::Foundation::HWND,
) -> Result<(), String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_CONTROL, VK_MENU,
        VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetWindowThreadProcessId, SendMessageW, SetForegroundWindow,
    };
    const VK_LWIN: VIRTUAL_KEY = VIRTUAL_KEY(0x5B);
    const VK_RWIN: VIRTUAL_KEY = VIRTUAL_KEY(0x5C);
    const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);
    const WM_PASTE: u32 = 0x0302;
    unsafe fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        let scan = MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) as u16;
        let mut flags = KEYEVENTF_SCANCODE;
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    unsafe {
        let target = if !window.0.is_null() {
            window
        } else {
            focus
        };
        if target.0.is_null() {
            return Err("no target window".into());
        }
        let field = if !focus.0.is_null() { focus } else { target };
        std::thread::sleep(std::time::Duration::from_millis(30));
        let our = GetCurrentThreadId();
        let tid = GetWindowThreadProcessId(target, None);
        type AttachFn = unsafe extern "system" fn(u32, u32, i32) -> i32;
        let attach: Option<AttachFn> = GetModuleHandleW(windows::core::w!("user32.dll"))
            .ok()
            .and_then(|lib| GetProcAddress(lib, windows::core::s!("AttachThreadInput")))
            .map(|f| std::mem::transmute(f));
        let attached = tid != 0
            && tid != our
            && attach.map(|f| f(our, tid, 1) != 0).unwrap_or(false);
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        let _ = SetFocus(field);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let _ = SendMessageW(field, WM_PASTE, WPARAM(0), LPARAM(0));
        let ups = [
            key(VK_MENU, true),
            key(VK_SHIFT, true),
            key(VK_CONTROL, true),
            key(VK_LWIN, true),
            key(VK_RWIN, true),
        ];
        let _ = SendInput(&ups, std::mem::size_of::<INPUT>() as i32);
        let seq = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        let n = SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
        if attached {
            if let Some(f) = attach {
                let _ = f(our, tid, 0);
            }
        }
        if n == 0 {
            return Err("SendInput failed".into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn paste_into_hwnd(_window: isize, _focus: isize) -> Result<(), String> {
    Err("paste is Windows-only".into())
}

#[cfg(windows)]
unsafe fn bitblt_rect(r: PhysRect) -> Result<Frame, String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        ReleaseDC, SelectObject, SRCCOPY,
    };
    let desktop = HWND(std::ptr::null_mut());
    let hdc_scr = GetDC(desktop);
    if hdc_scr.0.is_null() {
        return Err("GetDC failed".into());
    }
    let hdc_mem = CreateCompatibleDC(hdc_scr);
    let hbmp = CreateCompatibleBitmap(hdc_scr, r.w, r.h);
    let old = SelectObject(hdc_mem, hbmp);
    BitBlt(hdc_mem, 0, 0, r.w, r.h, hdc_scr, r.x, r.y, SRCCOPY).map_err(|e| e.to_string())?;
    let bgra = read_dib_section(hdc_mem, hbmp, r.w, r.h)?;
    SelectObject(hdc_mem, old);
    let _ = DeleteObject(hbmp);
    let _ = DeleteDC(hdc_mem);
    ReleaseDC(desktop, hdc_scr);
    Ok(Frame { rect: r, bgra })
}

#[cfg(windows)]
unsafe fn read_dib_section(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    hbmp: windows::Win32::Graphics::Gdi::HBITMAP,
    w: i32,
    h: i32,
) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Gdi::{
        GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    let mut info = BITMAPINFO {
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
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    let got = GetDIBits(
        hdc,
        hbmp,
        0,
        h as u32,
        Some(buf.as_mut_ptr().cast()),
        &mut info,
        DIB_RGB_COLORS,
    );
    if got == 0 {
        return Err("GetDIBits failed".into());
    }
    Ok(buf)
}

/// Prefer real text over a bitmap preview (Word/Chrome often put both).
#[derive(Debug)]
pub enum ClipPayload {
    Text(String),
    Image(Vec<u8>),
}

pub fn pick_clip_payload(
    text: Option<String>,
    png: Option<Vec<u8>>,
    dib: Option<Vec<u8>>,
) -> Option<ClipPayload> {
    if let Some(t) = text {
        let t = t.trim_end_matches('\0').to_string();
        if !t.trim().is_empty() {
            return Some(ClipPayload::Text(t));
        }
    }
    katana_shot::png_from_clip_sources(png.as_deref(), dib.as_deref()).map(ClipPayload::Image)
}

#[cfg(windows)]
pub fn set_clipboard_png_dib(png: &[u8], dib: &[u8]) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::core::w;
    unsafe fn to_hglobal(bytes: &[u8]) -> Result<windows::Win32::Foundation::HGLOBAL, String> {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
        let p = GlobalLock(h);
        if p.is_null() {
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast(), bytes.len());
        let _ = GlobalUnlock(h);
        Ok(h)
    }
    unsafe {
        OpenClipboard(HWND(std::ptr::null_mut())).map_err(|e| e.to_string())?;
        let _ = EmptyClipboard();
        let h_dib = to_hglobal(dib)?;
        const CF_DIB: u32 = 8;
        SetClipboardData(CF_DIB, windows::Win32::Foundation::HANDLE(h_dib.0))
            .map_err(|e| e.to_string())?;
        let fmt = RegisterClipboardFormatW(w!("PNG"));
        if fmt != 0 {
            let h_png = to_hglobal(png)?;
            let _ = SetClipboardData(fmt, windows::Win32::Foundation::HANDLE(h_png.0));
        }
        let _ = CloseClipboard();
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_clipboard_png_dib(_png: &[u8], _dib: &[u8]) -> Result<(), String> {
    Err("clipboard write is Windows-only".into())
}

#[cfg(windows)]
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let mut wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let bytes = wide.len() * 2;
    unsafe {
        OpenClipboard(HWND(std::ptr::null_mut())).map_err(|e| e.to_string())?;
        let _ = EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| {
            let _ = CloseClipboard();
            e.to_string()
        })?;
        let p = GlobalLock(h);
        if p.is_null() {
            let _ = CloseClipboard();
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_mut_ptr(), p.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(h);
        let result = SetClipboardData(CF_UNICODETEXT, windows::Win32::Foundation::HANDLE(h.0))
            .map_err(|e| e.to_string());
        let _ = CloseClipboard();
        result.map(|_| ())
    }
}

#[cfg(not(windows))]
pub fn set_clipboard_text(_text: &str) -> Result<(), String> {
    Err("clipboard write is Windows-only".into())
}

pub fn clamp_region(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<PhysRect> {
    let x = x0.min(x1);
    let y = y0.min(y1);
    let w = (x0 - x1).unsigned_abs() as i32;
    let h = (y0 - y1).unsigned_abs() as i32;
    if w < 2 || h < 2 {
        return None;
    }
    Some(PhysRect { x, y, w, h, dpi: 96 })
}

/// Copy a sub-rectangle out of an already-captured frame.
pub fn crop_frame(full: &Frame, r: PhysRect) -> Option<Frame> {
    let ox = r.x - full.rect.x;
    let oy = r.y - full.rect.y;
    if r.w <= 0 || r.h <= 0 || ox < 0 || oy < 0 {
        return None;
    }
    if ox + r.w > full.rect.w || oy + r.h > full.rect.h {
        return None;
    }
    let src_stride = (full.rect.w as usize) * 4;
    let dst_stride = (r.w as usize) * 4;
    let mut bgra = vec![0u8; dst_stride * r.h as usize];
    for row in 0..r.h as usize {
        let src = (oy as usize + row) * src_stride + (ox as usize) * 4;
        let dst = row * dst_stride;
        bgra[dst..dst + dst_stride].copy_from_slice(&full.bgra[src..src + dst_stride]);
    }
    Some(Frame { rect: r, bgra })
}

pub fn dim_bgra(src: &[u8], num: u16, den: u16) -> Vec<u8> {
    let den = den.max(1);
    let mut out = src.to_vec();
    for px in out.chunks_exact_mut(4) {
        px[0] = ((px[0] as u16 * num) / den) as u8;
        px[1] = ((px[1] as u16 * num) / den) as u8;
        px[2] = ((px[2] as u16 * num) / den) as u8;
    }
    out
}

#[cfg(windows)]
fn select_and_capture_region() -> Result<Frame, String> {
    // Capture first so the picker overlay is not in the shot.
    let full = capture_virtual_screen()?;
    let rect = pick_region(&full)?;
    crop_frame(&full, rect).ok_or_else(|| "selection out of bounds".into())
}

#[cfg(not(windows))]
fn select_and_capture_region() -> Result<Frame, String> {
    Err("region select is Windows-only".into())
}

#[cfg(windows)]
fn pick_region(full: &Frame) -> Result<PhysRect, String> {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreatePen, DeleteDC,
        DeleteObject, EndPaint, InvalidateRect, LineTo, MoveToEx, SelectObject, SetBkMode,
        SetTextColor, TextOutW, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, PAINTSTRUCT,
        PS_SOLID, SRCCOPY, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
        GetMessageW, GetSystemMetrics, GetWindowLongPtrW, LoadCursorW, PostQuitMessage,
        RegisterClassW, SetCursor, SetForegroundWindow, SetWindowLongPtrW, ShowWindow,
        TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_CROSS, MSG,
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW,
        WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR,
        WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};

    struct PickGdi {
        hdc_orig: windows::Win32::Graphics::Gdi::HDC,
        hdc_dim: windows::Win32::Graphics::Gdi::HDC,
        bmp_orig: windows::Win32::Graphics::Gdi::HBITMAP,
        bmp_dim: windows::Win32::Graphics::Gdi::HBITMAP,
        old_orig: windows::Win32::Graphics::Gdi::HGDIOBJ,
        old_dim: windows::Win32::Graphics::Gdi::HGDIOBJ,
        vw: i32,
        vh: i32,
    }

    impl Drop for PickGdi {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.hdc_orig, self.old_orig);
                SelectObject(self.hdc_dim, self.old_dim);
                let _ = DeleteObject(self.bmp_orig);
                let _ = DeleteObject(self.bmp_dim);
                let _ = DeleteDC(self.hdc_orig);
                let _ = DeleteDC(self.hdc_dim);
            }
        }
    }

    static X0: AtomicI32 = AtomicI32::new(0);
    static Y0: AtomicI32 = AtomicI32::new(0);
    static X1: AtomicI32 = AtomicI32::new(0);
    static Y1: AtomicI32 = AtomicI32::new(0);
    static DRAG: AtomicBool = AtomicBool::new(false);
    static DONE: AtomicBool = AtomicBool::new(false);
    static CANCEL: AtomicBool = AtomicBool::new(false);
    static OX: AtomicI32 = AtomicI32::new(0);
    static OY: AtomicI32 = AtomicI32::new(0);

    unsafe fn bgra_dc(
        src_hdc: windows::Win32::Graphics::Gdi::HDC,
        w: i32,
        h: i32,
        bgra: &[u8],
    ) -> Result<
        (
            windows::Win32::Graphics::Gdi::HDC,
            windows::Win32::Graphics::Gdi::HBITMAP,
            windows::Win32::Graphics::Gdi::HGDIOBJ,
        ),
        String,
    > {
        let hdc = CreateCompatibleDC(src_hdc);
        if hdc.0.is_null() {
            return Err("CreateCompatibleDC failed".into());
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
        let mut bits = std::ptr::null_mut();
        let hbmp = CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| e.to_string())?;
        if !bits.is_null() && !bgra.is_empty() {
            let n = bgra.len().min((w as usize) * (h as usize) * 4);
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast(), n);
        }
        let old = SelectObject(hdc, hbmp);
        Ok((hdc, hbmp, old))
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match msg {
            m if m == WM_SETCURSOR => {
                if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
                    let _ = SetCursor(c);
                }
                LRESULT(1)
            }
            WM_LBUTTONDOWN => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                X0.store(pt.x, Ordering::SeqCst);
                Y0.store(pt.y, Ordering::SeqCst);
                X1.store(pt.x, Ordering::SeqCst);
                Y1.store(pt.y, Ordering::SeqCst);
                DRAG.store(true, Ordering::SeqCst);
                SetCapture(hwnd);
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                X1.store(pt.x, Ordering::SeqCst);
                Y1.store(pt.y, Ordering::SeqCst);
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if DRAG.swap(false, Ordering::SeqCst) {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    X1.store(pt.x, Ordering::SeqCst);
                    Y1.store(pt.y, Ordering::SeqCst);
                    let _ = ReleaseCapture();
                    DONE.store(true, Ordering::SeqCst);
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN if w.0 as u16 == 0x1B => {
                CANCEL.store(true, Ordering::SeqCst);
                let _ = ReleaseCapture();
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    let gdi = &*(ptr as *const PickGdi);
                    let _ = BitBlt(hdc, 0, 0, gdi.vw, gdi.vh, gdi.hdc_dim, 0, 0, SRCCOPY);
                    let ox = OX.load(Ordering::SeqCst);
                    let oy = OY.load(Ordering::SeqCst);
                    let mx = X1.load(Ordering::SeqCst) - ox;
                    let my = Y1.load(Ordering::SeqCst) - oy;
                    if DRAG.load(Ordering::SeqCst) {
                        let ax = X0.load(Ordering::SeqCst) - ox;
                        let ay = Y0.load(Ordering::SeqCst) - oy;
                        let x = ax.min(mx);
                        let y = ay.min(my);
                        let x2 = ax.max(mx);
                        let y2 = ay.max(my);
                        let rw = (x2 - x).max(0);
                        let rh = (y2 - y).max(0);
                        if rw > 0 && rh > 0 {
                            let _ = BitBlt(hdc, x, y, rw, rh, gdi.hdc_orig, x, y, SRCCOPY);
                        }
                        let gold = CreatePen(PS_SOLID, 2, COLORREF(0x0024_A5F5));
                        let old = SelectObject(hdc, gold);
                        let _ = MoveToEx(hdc, x, y, None);
                        let _ = LineTo(hdc, x2, y);
                        let _ = LineTo(hdc, x2, y2);
                        let _ = LineTo(hdc, x, y2);
                        let _ = LineTo(hdc, x, y);
                        SelectObject(hdc, old);
                        let _ = DeleteObject(gold);
                        SetBkMode(hdc, TRANSPARENT);
                        SetTextColor(hdc, COLORREF(0x00F4_F4F5));
                        let label = format!("{rw} × {rh}");
                        let wlabel: Vec<u16> = label.encode_utf16().collect();
                        let _ = TextOutW(hdc, x + 8, y + 8, &wlabel);
                    } else {
                        SetBkMode(hdc, TRANSPARENT);
                        SetTextColor(hdc, COLORREF(0x00F4_F4F5));
                        let hint: Vec<u16> =
                            "Drag to select a region   ·   Esc to cancel".encode_utf16().collect();
                        let _ = TextOutW(hdc, 24, 24, &hint);
                    }
                    let shadow = CreatePen(PS_SOLID, 3, COLORREF(0x0000_0000));
                    let old = SelectObject(hdc, shadow);
                    let _ = MoveToEx(hdc, mx, 0, None);
                    let _ = LineTo(hdc, mx, gdi.vh);
                    let _ = MoveToEx(hdc, 0, my, None);
                    let _ = LineTo(hdc, gdi.vw, my);
                    let gold = CreatePen(PS_SOLID, 1, COLORREF(0x0024_A5F5));
                    SelectObject(hdc, gold);
                    let _ = MoveToEx(hdc, mx, 0, None);
                    let _ = LineTo(hdc, mx, gdi.vh);
                    let _ = MoveToEx(hdc, 0, my, None);
                    let _ = LineTo(hdc, gdi.vw, my);
                    SelectObject(hdc, old);
                    let _ = DeleteObject(gold);
                    let _ = DeleteObject(shadow);
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    drop(Box::from_raw(ptr as *mut PickGdi));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }

    unsafe {
        DONE.store(false, Ordering::SeqCst);
        CANCEL.store(false, Ordering::SeqCst);
        DRAG.store(false, Ordering::SeqCst);
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        OX.store(vx, Ordering::SeqCst);
        OY.store(vy, Ordering::SeqCst);

        let desktop = windows::Win32::Graphics::Gdi::GetDC(HWND(std::ptr::null_mut()));
        let (hdc_orig, bmp_orig, old_orig) = bgra_dc(desktop, vw, vh, &full.bgra)?;
        let dimmed = dim_bgra(&full.bgra, 2, 5);
        let (hdc_dim, bmp_dim, old_dim) = bgra_dc(desktop, vw, vh, &dimmed)?;
        windows::Win32::Graphics::Gdi::ReleaseDC(HWND(std::ptr::null_mut()), desktop);

        let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("KatanaRegionPick");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(proc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Katana select"),
            WS_POPUP,
            vx,
            vy,
            vw,
            vh,
            None,
            None,
            hinst,
            None,
        )
        .map_err(|e| e.to_string())?;
        let gdi = Box::new(PickGdi {
            hdc_orig,
            hdc_dim,
            bmp_orig,
            bmp_dim,
            old_orig,
            old_dim,
            vw,
            vh,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(gdi) as isize);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
            let _ = SetCursor(c);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if CANCEL.load(Ordering::SeqCst) || !DONE.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    clamp_region(
        X0.load(Ordering::SeqCst),
        Y0.load(Ordering::SeqCst),
        X1.load(Ordering::SeqCst),
        Y1.load(Ordering::SeqCst),
    )
    .ok_or_else(|| "selection too small".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use katana_shot::{dib_from_bgra, png_from_bgra, png_to_bgra};

    #[test]
    fn capture_last_without_region_errors() {
        let err = capture(ShotMode::Last, None).unwrap_err();
        assert!(err.contains("last") || err.contains("Windows"), "{err}");
    }

    #[test]
    fn clamp_region_normalizes_and_rejects_tiny() {
        let r = clamp_region(100, 80, 20, 10).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (20, 10, 80, 70));
        assert!(clamp_region(5, 5, 6, 6).is_none());
    }

    #[test]
    fn crop_frame_extracts_subrect() {
        let mut bgra = vec![0u8; 4 * 2 * 4];
        let i = (1 * 4 + 1) * 4;
        bgra[i] = 11;
        bgra[i + 1] = 22;
        bgra[i + 2] = 33;
        bgra[i + 3] = 255;
        let full = Frame {
            rect: PhysRect {
                x: 10,
                y: 20,
                w: 4,
                h: 2,
                dpi: 96,
            },
            bgra,
        };
        let c = crop_frame(
            &full,
            PhysRect {
                x: 11,
                y: 21,
                w: 2,
                h: 1,
                dpi: 96,
            },
        )
        .unwrap();
        assert_eq!(c.rect.w, 2);
        assert_eq!(c.bgra[0], 11);
        assert_eq!(c.bgra[1], 22);
        assert!(crop_frame(
            &full,
            PhysRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                dpi: 96
            }
        )
        .is_none());
    }

    #[test]
    fn dim_bgra_scales_channels() {
        let src = vec![100u8, 50, 0, 255];
        let out = dim_bgra(&src, 2, 5);
        assert_eq!(out[0], 40);
        assert_eq!(out[1], 20);
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn clip_prefers_text_over_dib_preview() {
        let got = pick_clip_payload(
            Some(" copied text ".into()),
            None,
            Some(vec![0x89, b'P', b'N', b'G']),
        );
        match got {
            Some(ClipPayload::Text(t)) => assert!(t.contains("copied text")),
            other => panic!("{other:?}"),
        }
        let px = vec![1u8, 2, 3, 255];
        let png = png_from_bgra(1, 1, &px).unwrap();
        assert!(matches!(
            pick_clip_payload(Some("   ".into()), Some(png), None),
            Some(ClipPayload::Image(_))
        ));
        assert!(pick_clip_payload(None, None, None).is_none());
    }

    #[test]
    fn clip_image_from_dib_when_png_missing() {
        let px = vec![9u8, 8, 7, 255];
        let dib = dib_from_bgra(1, 1, &px).unwrap();
        match pick_clip_payload(None, None, Some(dib)) {
            Some(ClipPayload::Image(p)) => {
                let (_, _, back) = png_to_bgra(&p).unwrap();
                assert_eq!(back, px);
            }
            other => panic!("{other:?}"),
        }
    }
}
