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
    dib_png: Option<Vec<u8>>,
) -> Option<ClipPayload> {
    if let Some(t) = text {
        let t = t.trim_end_matches('\0').to_string();
        if !t.trim().is_empty() {
            return Some(ClipPayload::Text(t));
        }
    }
    if let Some(p) = png {
        if p.len() > 8 {
            return Some(ClipPayload::Image(p));
        }
    }
    dib_png.map(ClipPayload::Image)
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
        assert!(matches!(
            pick_clip_payload(Some("   ".into()), Some(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5]), None),
            Some(ClipPayload::Image(_))
        ));
        assert!(pick_clip_payload(None, None, None).is_none());
    }
}
