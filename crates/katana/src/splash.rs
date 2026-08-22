//! 3s branded splash on GUI start.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, EndPaint,
    FillRect, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TextOutW, TRANSPARENT,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, GetClientRect,
    GetSystemMetrics, LoadCursorW, PeekMessageW, RegisterClassW, SetForegroundWindow, ShowWindow,
    TranslateMessage, CS_DROPSHADOW, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, MSG,
    PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONUP, WM_PAINT,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, DI_NORMAL,
};

const W: i32 = 360;
const H: i32 = 220;
const BG: u32 = 0x0014_1416;
const TEXT: u32 = 0x00F4_F4F5;
const MUTED: u32 = 0x00A1_A1AA;
const ACCENT: u32 = 0x0024_A5F5;

static DISMISSED: AtomicBool = AtomicBool::new(false);
static ICON: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(COLORREF(BG));
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(brush);

            let icon = windows::Win32::UI::WindowsAndMessaging::HICON(ICON.load(Ordering::SeqCst) as *mut _);
            if !icon.0.is_null() {
                let _ = DrawIconEx(hdc, (W - 72) / 2, 28, icon, 72, 72, 0, None, DI_NORMAL);
            }

            SetBkMode(hdc, TRANSPARENT);
            let title = CreateFontW(
                -28,
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                w!("Segoe UI"),
            );
            let old = SelectObject(hdc, title);
            SetTextColor(hdc, COLORREF(TEXT));
            let name: Vec<u16> = "Katana".encode_utf16().collect();
            let _ = TextOutW(hdc, 132, 112, &name);

            let small = CreateFontW(
                -14,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                w!("Segoe UI"),
            );
            SelectObject(hdc, small);
            SetTextColor(hdc, COLORREF(MUTED));
            let hint: Vec<u16> = "fast  ·  small  ·  many talents".encode_utf16().collect();
            let _ = TextOutW(hdc, 72, 156, &hint);
            SetTextColor(hdc, COLORREF(ACCENT));
            let tag: Vec<u16> = "swiss knife".encode_utf16().collect();
            let _ = TextOutW(hdc, 128, 176, &tag);

            SelectObject(hdc, old);
            let _ = DeleteObject(title);
            let _ = DeleteObject(small);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_KEYDOWN => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            DISMISSED.store(true, Ordering::SeqCst);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

pub struct Splash {
    hwnd: HWND,
    start: Instant,
}

impl Splash {
    pub fn show() -> Option<Self> {
        DISMISSED.store(false, Ordering::SeqCst);
        unsafe {
            let icon = crate::ui::load_app_icon_size(72, 72);
            ICON.store(icon.0 as isize, Ordering::SeqCst);
            let hinst = GetModuleHandleW(None).ok()?;
            let class = w!("KatanaSplash");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
                lpfnWndProc: Some(proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: icon,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class,
                w!("Katana"),
                WS_POPUP,
                (sw - W) / 2,
                (sh - H) / 2,
                W,
                H,
                None,
                None,
                hinst,
                None,
            )
            .ok()?;
            let rgn = CreateRoundRectRgn(0, 0, W + 1, H + 1, 20, 20);
            let _ = SetWindowRgn(hwnd, rgn, true);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            pump(Duration::from_millis(16));
            Some(Self {
                hwnd,
                start: Instant::now(),
            })
        }
    }

    /// Stay visible until `SPLASH_MS` from show (or click / key).
    pub fn finish(self) {
        let leftover = Duration::from_millis(SPLASH_MS).saturating_sub(self.start.elapsed());
        pump(leftover);
        unsafe {
            if !DISMISSED.load(Ordering::SeqCst) {
                let _ = DestroyWindow(self.hwnd);
                pump(Duration::from_millis(16));
            }
        }
    }
}

fn pump(budget: Duration) {
    let deadline = Instant::now() + budget;
    let mut msg = MSG::default();
    while Instant::now() < deadline && !DISMISSED.load(Ordering::SeqCst) {
        unsafe {
            if PeekMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0, PM_REMOVE).as_bool() {
                if msg.message == 0x0012 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                std::thread::sleep(Duration::from_millis(12));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn splash_duration_constant_is_3000() {
        assert_eq!(super::SPLASH_MS, 3000);
    }
}

pub const SPLASH_MS: u64 = 3000;
