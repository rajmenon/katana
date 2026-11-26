//! Screen recording. Capture and H.264 encode run on a worker thread.
//! The UI thread only starts, stops, and paints the indicator.
//!
//! Desktop Duplication supplies frames. Media Foundation's sink writer
//! muxes H.264 (and optional AAC) into an MP4. No ffmpeg binary.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub const WM_REC_BEGAN: u32 = 0x8010;
pub const WM_REC_DONE: u32 = 0x8011;
pub const WM_REC_FAIL: u32 = 0x8012;

const AUDIO_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecEventKind {
    Began,
    Saved,
    Failed,
}

#[derive(Debug, Clone)]
pub struct RecEvent {
    pub kind: RecEventKind,
    pub path: PathBuf,
    pub detail: String,
    /// Screen pixels being encoded, `(x, y, w, h)`. Zero size if unknown.
    pub bounds: (i32, i32, i32, i32),
}

fn rec_event(
    kind: RecEventKind,
    path: PathBuf,
    detail: impl Into<String>,
    bounds: (i32, i32, i32, i32),
) -> RecEvent {
    RecEvent {
        kind,
        path,
        detail: detail.into(),
        bounds,
    }
}

#[derive(Debug, Clone)]
pub struct RecJob {
    pub fps: u32,
    pub system_audio: bool,
    pub mic: bool,
    pub output_dir: PathBuf,
    pub pick_region: bool,
}

static EVENTS: Mutex<VecDeque<RecEvent>> = Mutex::new(VecDeque::new());

#[cfg(windows)]
static RUNNING: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static STOP: AtomicBool = AtomicBool::new(false);

pub fn pop_event() -> Option<RecEvent> {
    EVENTS.lock().ok()?.pop_front()
}

pub fn is_recording() -> bool {
    #[cfg(windows)]
    {
        RUNNING.load(Ordering::SeqCst)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

fn even_extent(v: i32) -> u32 {
    if v < 2 {
        0
    } else {
        (v as u32) & !1
    }
}

/// Output-local crop with even width and height, as H.264 requires.
pub fn frame_on_monitor(mon: MonitorRect, region: Option<(i32, i32, i32, i32)>) -> Option<FrameBox> {
    let (x, y, w, h) = match region {
        None => (mon.x, mon.y, mon.w, mon.h),
        Some((rx, ry, rw, rh)) => {
            let x1 = rx.max(mon.x);
            let y1 = ry.max(mon.y);
            let x2 = (rx + rw).min(mon.x + mon.w);
            let y2 = (ry + rh).min(mon.y + mon.h);
            (x1, y1, x2 - x1, y2 - y1)
        }
    };
    let local_x = (x - mon.x).max(0) as u32;
    let local_y = (y - mon.y).max(0) as u32;
    let max_w = (mon.w.max(0) as u32).saturating_sub(local_x) & !1;
    let max_h = (mon.h.max(0) as u32).saturating_sub(local_y) & !1;
    let ew = even_extent(w).min(max_w);
    let eh = even_extent(h).min(max_h);
    if ew < 2 || eh < 2 {
        return None;
    }
    Some(FrameBox {
        x: local_x,
        y: local_y,
        w: ew,
        h: eh,
    })
}

/// Region: monitor with the largest overlap. Full screen: monitor under the cursor.
pub fn pick_monitor(
    mons: &[MonitorRect],
    cursor: Option<(i32, i32)>,
    region: Option<(i32, i32, i32, i32)>,
) -> Option<usize> {
    if mons.is_empty() {
        return None;
    }
    if let Some((rx, ry, rw, rh)) = region {
        let mut best = None;
        let mut best_area = 0i64;
        for (i, m) in mons.iter().enumerate() {
            let x1 = rx.max(m.x);
            let y1 = ry.max(m.y);
            let x2 = (rx + rw).min(m.x + m.w);
            let y2 = (ry + rh).min(m.y + m.h);
            let area = (x2 - x1).max(0) as i64 * (y2 - y1).max(0) as i64;
            if area > best_area {
                best_area = area;
                best = Some(i);
            }
        }
        return best;
    }
    if let Some((cx, cy)) = cursor {
        for (i, m) in mons.iter().enumerate() {
            if cx >= m.x && cy >= m.y && cx < m.x + m.w && cy < m.y + m.h {
                return Some(i);
            }
        }
    }
    Some(0)
}

pub fn h264_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let fps = fps.max(1);
    let pixels = (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(fps as u64);
    (pixels / 8).clamp(400_000, 20_000_000) as u32
}

/// H.264 level_idc rough enough for the inbox encoder.
/// Red stroke just outside the hole, then a short dim halo beyond that.
pub const OUTLINE_RED: i32 = 2;
pub const OUTLINE_DIM: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outline {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Transparent hole, in window coordinates. That hole is the captured rect.
    pub hole_x: i32,
    pub hole_y: i32,
    pub hole_w: i32,
    pub hole_h: i32,
}

/// Frame around the captured pixels. Each side sits just outside the capture
/// when the virtual screen has room, and on the inner edge when it does not
/// (a full-monitor recording, where "outside" would be off the screen).
pub fn outline_geometry(
    cap_x: i32,
    cap_y: i32,
    cap_w: i32,
    cap_h: i32,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
) -> Outline {
    let (x, hole_x, w, hole_w) = outline_axis(cap_x, cap_w, vx, vw);
    let (y, hole_y, h, hole_h) = outline_axis(cap_y, cap_h, vy, vh);
    Outline {
        x,
        y,
        w,
        h,
        hole_x,
        hole_y,
        hole_w,
        hole_h,
    }
}

fn outline_axis(cap: i32, len: i32, screen: i32, screen_len: i32) -> (i32, i32, i32, i32) {
    let len = len.max(0);
    let screen_end = screen.saturating_add(screen_len);
    let cap_end = cap.saturating_add(len);
    let (before, inside_before) = if cap - (OUTLINE_RED + OUTLINE_DIM) >= screen {
        (OUTLINE_RED + OUTLINE_DIM, 0)
    } else if cap - OUTLINE_RED >= screen {
        (OUTLINE_RED, 0)
    } else {
        (0, OUTLINE_RED.min(len / 2))
    };
    let (after, inside_after) = if cap_end + OUTLINE_RED + OUTLINE_DIM <= screen_end {
        (OUTLINE_RED + OUTLINE_DIM, 0)
    } else if cap_end + OUTLINE_RED <= screen_end {
        (OUTLINE_RED, 0)
    } else {
        (0, OUTLINE_RED.min((len - inside_before) / 2))
    };
    let win = cap - before;
    let win_len = (before + len + after).max(1);
    let hole = before + inside_before;
    let hole_len = (len - inside_before - inside_after).max(0);
    (win, hole, win_len, hole_len)
}

/// Premultiplied BGRA for one overlay pixel. The hole is clear. The next
/// 2px are red, then a short dim halo. Corners keep a slightly longer red mark.
pub fn outline_pixel(
    x: i32,
    y: i32,
    hole_x: i32,
    hole_y: i32,
    hole_w: i32,
    hole_h: i32,
) -> [u8; 4] {
    let Some(dist) = outside_hole(x, y, hole_x, hole_y, hole_w, hole_h) else {
        return [0, 0, 0, 0];
    };
    let corner = near_hole_corner(x, y, hole_x, hole_y, hole_w, hole_h);
    let red_px = if corner { 4 } else { OUTLINE_RED };
    if dist <= red_px {
        // #E11D48, opaque, premultiplied.
        [0x48, 0x1D, 0xE1, 0xFF]
    } else if dist <= OUTLINE_RED + OUTLINE_DIM {
        // Soft black halo, alpha 64.
        [0, 0, 0, 64]
    } else {
        [0, 0, 0, 0]
    }
}

fn outside_hole(x: i32, y: i32, hx: i32, hy: i32, hw: i32, hh: i32) -> Option<i32> {
    if hw <= 0 || hh <= 0 {
        return Some(99);
    }
    let right = hx + hw;
    let bottom = hy + hh;
    if x >= hx && x < right && y >= hy && y < bottom {
        return None;
    }
    let dx = if x < hx {
        hx - x
    } else if x >= right {
        x - (right - 1)
    } else {
        0
    };
    let dy = if y < hy {
        hy - y
    } else if y >= bottom {
        y - (bottom - 1)
    } else {
        0
    };
    Some(dx.max(dy))
}

fn near_hole_corner(x: i32, y: i32, hx: i32, hy: i32, hw: i32, hh: i32) -> bool {
    let right = hx + hw - 1;
    let bottom = hy + hh - 1;
    [(hx, hy), (right, hy), (hx, bottom), (right, bottom)]
        .into_iter()
        .any(|(cx, cy)| (x - cx).abs() <= 12 && (y - cy).abs() <= 12)
}

pub fn h264_level_idc(width: u32, height: u32) -> u32 {
    let px = (width as u64).saturating_mul(height as u64);
    if px <= 1280 * 720 {
        31
    } else if px <= 1920 * 1088 {
        40
    } else {
        51
    }
}

/// Limited-range BT.601 NV12. `bgra` is tightly packed.
pub fn bgra_to_nv12(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let w = width as usize;
    let h = height as usize;
    if w % 2 != 0 || h % 2 != 0 {
        return Err("even dimensions required".into());
    }
    let need = w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or("overflow")?;
    if bgra.len() < need {
        return Err("short frame".into());
    }
    let mut out = vec![0u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let b = bgra[i] as i32;
            let g = bgra[i + 1] as i32;
            let r = bgra[i + 2] as i32;
            let yy = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            out[y * w + x] = yy.clamp(0, 255) as u8;
        }
    }
    let y_plane = w * h;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let mut r = 0i32;
            let mut g = 0i32;
            let mut b = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((y + dy) * w + (x + dx)) * 4;
                    b += bgra[i] as i32;
                    g += bgra[i + 1] as i32;
                    r += bgra[i + 2] as i32;
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            let uv = y_plane + (y / 2) * w + x;
            out[uv] = u.clamp(0, 255) as u8;
            out[uv + 1] = v.clamp(0, 255) as u8;
        }
    }
    Ok(out)
}

pub fn mix_i16(a: &[i16], b: &[i16], n: usize) -> Vec<i16> {
    let mut out = vec![0i16; n];
    for i in 0..n {
        let av = *a.get(i).unwrap_or(&0) as i32;
        let bv = *b.get(i).unwrap_or(&0) as i32;
        out[i] = (av + bv).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    out
}

/// Streaming resampler to 48 kHz stereo. `phase` is the next output's position
/// in input frames; `prev` is the sample before this chunk.
pub fn resample_to_48k(
    prev: &mut Option<(f32, f32)>,
    phase: &mut f64,
    input: &[(f32, f32)],
    in_rate: u32,
    out: &mut Vec<i16>,
) {
    if input.is_empty() || in_rate == 0 {
        return;
    }
    let step = in_rate as f64 / AUDIO_RATE as f64;
    let n = input.len() as f64;
    while *phase < n {
        let (l, r) = sample_at(*prev, input, *phase);
        out.push(f32_to_i16(l));
        out.push(f32_to_i16(r));
        *phase += step;
    }
    *phase -= n;
    *prev = Some(input[input.len() - 1]);
}

fn sample_at(prev: Option<(f32, f32)>, input: &[(f32, f32)], pos: f64) -> (f32, f32) {
    if pos < 0.0 {
        let p = prev.unwrap_or(input[0]);
        let t = (pos + 1.0).clamp(0.0, 1.0) as f32;
        return lerp(p, input[0], t);
    }
    let i = (pos.floor() as usize).min(input.len() - 1);
    let t = (pos - i as f64) as f32;
    let b = input[(i + 1).min(input.len() - 1)];
    lerp(input[i], b, t.clamp(0.0, 1.0))
}

fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Instant;

    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
        IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
        DXGI_OUTDUPL_FRAME_INFO,
    };
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
    };
    use windows::Win32::Media::MediaFoundation::{
        eAVEncH264VProfile_Main, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFCreateSinkWriterFromURL, MFStartup, IMFMediaBuffer, IMFMediaType, IMFSample,
        IMFSinkWriter, MFAudioFormat_AAC, MFAudioFormat_PCM, MFMediaType_Audio, MFMediaType_Video,
        MFSTARTUP_LITE, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
        MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
        MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
        MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
        MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE,
        MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, PostMessageW};

    enum SessionEnd {
        Quiet,
        Fail(String),
        Saved { path: PathBuf, detail: String },
    }

    enum AudioState {
        Starting,
        Ok,
        Err(String),
    }

    struct AudioFeed {
        q: Mutex<VecDeque<i16>>,
        state: Mutex<AudioState>,
    }

    struct AudioGuard {
        handles: Vec<JoinHandle<()>>,
    }

    impl Drop for AudioGuard {
        fn drop(&mut self) {
            STOP.store(true, Ordering::SeqCst);
            for h in self.handles.drain(..) {
                let _ = h.join();
            }
        }
    }

    pub fn start(hwnd: HWND, job: RecJob) -> Result<(), String> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err("Already recording".into());
        }
        STOP.store(false, Ordering::SeqCst);
        let hwnd_bits = hwnd.0 as isize;
        match std::thread::Builder::new()
            .name("katana-rec".into())
            .spawn(move || {
                let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session(hwnd_bits, job)));
                RUNNING.store(false, Ordering::SeqCst);
                match end {
                    Ok(SessionEnd::Quiet) => {}
                    Ok(SessionEnd::Saved { path, detail }) => {
                        push_event(
                            hwnd_bits,
                            WM_REC_DONE,
                            rec_event(RecEventKind::Saved, path, detail, (0, 0, 0, 0)),
                        );
                    }
                    Ok(SessionEnd::Fail(detail)) => {
                        push_event(
                            hwnd_bits,
                            WM_REC_FAIL,
                            rec_event(RecEventKind::Failed, PathBuf::new(), detail, (0, 0, 0, 0)),
                        );
                    }
                    Err(_) => {
                        push_event(
                            hwnd_bits,
                            WM_REC_FAIL,
                            rec_event(
                                RecEventKind::Failed,
                                PathBuf::new(),
                                "recording stopped unexpectedly",
                                (0, 0, 0, 0),
                            ),
                        );
                    }
                }
            }) {
            Ok(_) => Ok(()),
            Err(e) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(e.to_string())
            }
        }
    }

    pub fn request_stop() {
        STOP.store(true, Ordering::SeqCst);
    }

    fn push_event(hwnd: isize, msg: u32, ev: RecEvent) {
        if let Ok(mut q) = EVENTS.lock() {
            q.push_back(ev);
        }
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(HWND(hwnd as *mut _), msg, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn same_hr(err: &windows::core::Error, hr: windows::core::HRESULT) -> bool {
        err.code() == hr
    }

    fn session(hwnd: isize, job: RecJob) -> SessionEnd {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
                return SessionEnd::Fail(format!("media foundation: {e}"));
            }
        }
        // Let the overlay finish hiding so it is not in the first frames.
        std::thread::sleep(Duration::from_millis(40));
        if STOP.load(Ordering::SeqCst) {
            return SessionEnd::Quiet;
        }
        let region = if job.pick_region {
            match crate::capture::select_region() {
                Ok(r) => Some((r.x, r.y, r.w, r.h)),
                Err(e) if e == "cancelled" => return SessionEnd::Quiet,
                Err(e) => return SessionEnd::Fail(e),
            }
        } else {
            None
        };
        if STOP.load(Ordering::SeqCst) {
            return SessionEnd::Quiet;
        }

        let cursor = unsafe {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_ok() {
                Some((pt.x, pt.y))
            } else {
                None
            }
        };

        let mut capture = match open_capture(region, cursor) {
            Ok(c) => c,
            Err(e) => return SessionEnd::Fail(e),
        };

        let mut guard = AudioGuard { handles: Vec::new() };
        let mut feeds: Vec<Arc<AudioFeed>> = Vec::new();
        if job.system_audio {
            if let Some((feed, handle)) = spawn_audio(true) {
                feeds.push(feed);
                guard.handles.push(handle);
            }
        }
        if job.mic {
            if let Some((feed, handle)) = spawn_audio(false) {
                feeds.push(feed);
                guard.handles.push(handle);
            }
        }
        let mut audio_errors = Vec::new();
        let mut live: Vec<Arc<AudioFeed>> = Vec::new();
        for feed in &feeds {
            match wait_audio(feed) {
                Ok(()) => live.push(feed.clone()),
                Err(e) => audio_errors.push(e),
            }
        }
        let want_audio = job.system_audio || job.mic;
        let have_audio = !live.is_empty();

        if let Err(e) = std::fs::create_dir_all(&job.output_dir) {
            return SessionEnd::Fail(format!("output folder: {e}"));
        }
        let stamp = crate::persist::local_rec_stamp();
        let path = crate::persist::rec_output_path(&job.output_dir, stamp);
        let fps = job.fps.clamp(1, 60);

        let writer = match open_writer(&path, capture.width, capture.height, fps, have_audio) {
            Ok(w) => w,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return SessionEnd::Fail(e);
            }
        };

        push_event(
            hwnd,
            WM_REC_BEGAN,
            rec_event(
                RecEventKind::Began,
                path.clone(),
                "",
                (
                    capture.screen_x,
                    capture.screen_y,
                    capture.width as i32,
                    capture.height as i32,
                ),
            ),
        );

        let mut last = match capture_first(&mut capture) {
            Ok(f) => f,
            Err(e) if e == "cancelled" || STOP.load(Ordering::SeqCst) => {
                let _ = std::fs::remove_file(&path);
                // The border is already up; a silent failure takes it down.
                return SessionEnd::Fail("cancelled".into());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return SessionEnd::Fail(e);
            }
        };

        let period = Duration::from_nanos(1_000_000_000 / fps as u64);
        let mut next = Instant::now() + period;
        let mut frame_i: i64 = 0;
        let mut audio_samples: i64 = 0;
        let mut early = None;
        loop {
            if STOP.load(Ordering::SeqCst) {
                break;
            }
            match capture.latest() {
                Ok(Some(frame)) => last = frame,
                Ok(None) => {}
                Err(e) => {
                    early = Some(e);
                    break;
                }
            }
            let nv12 = match bgra_to_nv12(capture.width, capture.height, &last) {
                Ok(v) => v,
                Err(e) => {
                    early = Some(e);
                    break;
                }
            };
            let tick = 10_000_000 / fps as i64;
            if let Err(e) = write_sample(&writer.writer, writer.video, &nv12, frame_i * tick, tick) {
                early = Some(e);
                break;
            }
            if have_audio {
                let target = (frame_i + 1) * AUDIO_RATE as i64 / fps as i64;
                let need = ((target - audio_samples).max(0) as usize) * 2;
                if need > 0 {
                    let pcm = pull_mix(&live, need);
                    let n = pcm.len() / 2;
                    if n > 0 {
                        let at = audio_samples * 10_000_000 / AUDIO_RATE as i64;
                        let dur = n as i64 * 10_000_000 / AUDIO_RATE as i64;
                        if let Err(e) = write_sample(&writer.writer, writer.audio, &i16_bytes(&pcm), at, dur)
                        {
                            early = Some(e);
                            break;
                        }
                        audio_samples += n as i64;
                    }
                }
            }
            frame_i += 1;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            }
            next += period;
            if next + period < Instant::now() {
                next = Instant::now();
            }
        }

        drop(guard);
        let finalized = unsafe { writer.writer.Finalize() };
        if frame_i == 0 {
            let _ = std::fs::remove_file(&path);
            return SessionEnd::Fail(early.unwrap_or_else(|| "nothing captured".into()));
        }
        if let Err(e) = finalized {
            return SessionEnd::Fail(format!("could not finish the file: {e}"));
        }
        let mut detail = audio_detail(want_audio, have_audio, &audio_errors);
        if let Some(e) = early {
            detail = format!("{detail} · ended early: {e}");
        }
        SessionEnd::Saved { path, detail }
    }

    fn audio_detail(wanted: bool, have: bool, errors: &[String]) -> String {
        if !wanted {
            return "video only".into();
        }
        if have && errors.is_empty() {
            return "with audio".into();
        }
        if have {
            format!("with audio ({})", errors.join("; "))
        } else if errors.is_empty() {
            "video only".into()
        } else {
            format!("video only ({})", errors.join("; "))
        }
    }

    struct Writer {
        writer: IMFSinkWriter,
        video: u32,
        audio: u32,
    }

    fn open_writer(path: &std::path::Path, w: u32, h: u32, fps: u32, audio: bool) -> Result<Writer, String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let writer = unsafe {
            MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, None)
                .map_err(|e| format!("mp4: {e}"))?
        };
        let video = unsafe { add_video(&writer, w, h, fps)? };
        let audio_index = if audio {
            unsafe { add_audio(&writer)? }
        } else {
            0
        };
        unsafe { writer.BeginWriting().map_err(|e| format!("encoder: {e}"))? };
        Ok(Writer {
            writer,
            video,
            audio: audio_index,
        })
    }

    unsafe fn add_video(writer: &IMFSinkWriter, w: u32, h: u32, fps: u32) -> Result<u32, String> {
        let out_ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        set_guid(&out_ty, &MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        set_guid(&out_ty, &MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        set_u32(&out_ty, &MF_MT_AVG_BITRATE, h264_bitrate(w, h, fps))?;
        set_u32(&out_ty, &MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        set_u32(&out_ty, &MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
        set_u32(&out_ty, &MF_MT_MPEG2_LEVEL, h264_level_idc(w, h))?;
        set_ratio(&out_ty, &MF_MT_FRAME_SIZE, w, h)?;
        set_ratio(&out_ty, &MF_MT_FRAME_RATE, fps, 1)?;
        set_ratio(&out_ty, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
        let index = writer.AddStream(&out_ty).map_err(|e| format!("h264 stream: {e}"))?;

        let in_ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        set_guid(&in_ty, &MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        set_guid(&in_ty, &MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        set_u32(&in_ty, &MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        set_u32(&in_ty, &MF_MT_DEFAULT_STRIDE, w)?;
        set_ratio(&in_ty, &MF_MT_FRAME_SIZE, w, h)?;
        set_ratio(&in_ty, &MF_MT_FRAME_RATE, fps, 1)?;
        set_ratio(&in_ty, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
        writer
            .SetInputMediaType(index, &in_ty, None)
            .map_err(|e| format!("h264 input: {e}"))?;
        Ok(index)
    }

    unsafe fn add_audio(writer: &IMFSinkWriter) -> Result<u32, String> {
        let out_ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        set_guid(&out_ty, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        set_guid(&out_ty, &MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        set_u32(&out_ty, &MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        set_u32(&out_ty, &MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        set_u32(&out_ty, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        set_u32(&out_ty, &MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16000)?;
        set_u32(&out_ty, &MF_MT_AAC_PAYLOAD_TYPE, 0)?;
        let index = writer.AddStream(&out_ty).map_err(|e| format!("aac stream: {e}"))?;

        let in_ty = MFCreateMediaType().map_err(|e| e.to_string())?;
        set_guid(&in_ty, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        set_guid(&in_ty, &MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        set_u32(&in_ty, &MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
        set_u32(&in_ty, &MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        set_u32(&in_ty, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        set_u32(&in_ty, &MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
        set_u32(&in_ty, &MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_RATE * 4)?;
        writer
            .SetInputMediaType(index, &in_ty, None)
            .map_err(|e| format!("pcm input: {e}"))?;
        Ok(index)
    }

    unsafe fn set_guid(ty: &IMFMediaType, key: &windows::core::GUID, val: &windows::core::GUID) -> Result<(), String> {
        ty.SetGUID(key, val).map_err(|e| e.to_string())
    }

    unsafe fn set_u32(ty: &IMFMediaType, key: &windows::core::GUID, val: u32) -> Result<(), String> {
        ty.SetUINT32(key, val).map_err(|e| e.to_string())
    }

    unsafe fn set_ratio(ty: &IMFMediaType, key: &windows::core::GUID, hi: u32, lo: u32) -> Result<(), String> {
        let packed = ((hi as u64) << 32) | lo as u64;
        ty.SetUINT64(key, packed).map_err(|e| e.to_string())
    }

    fn i16_bytes(samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn write_sample(
        writer: &IMFSinkWriter,
        stream: u32,
        bytes: &[u8],
        time: i64,
        duration: i64,
    ) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        unsafe { write_sample_inner(writer, stream, bytes, time, duration) }
    }

    unsafe fn write_sample_inner(
        writer: &IMFSinkWriter,
        stream: u32,
        bytes: &[u8],
        time: i64,
        duration: i64,
    ) -> Result<(), String> {
        let buffer: IMFMediaBuffer = MFCreateMemoryBuffer(bytes.len() as u32).map_err(|e| e.to_string())?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None).map_err(|e| e.to_string())?;
        if ptr.is_null() {
            let _ = buffer.Unlock();
            return Err("encoder buffer".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        buffer.Unlock().map_err(|e| e.to_string())?;
        buffer.SetCurrentLength(bytes.len() as u32).map_err(|e| e.to_string())?;
        let sample: IMFSample = MFCreateSample().map_err(|e| e.to_string())?;
        sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
        sample.SetSampleTime(time).map_err(|e| e.to_string())?;
        sample.SetSampleDuration(duration.max(1)).map_err(|e| e.to_string())?;
        writer
            .WriteSample(stream, &sample)
            .map_err(|e| format!("write sample: {e}"))
    }

    struct Capture {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        output: IDXGIOutput1,
        dup: Option<IDXGIOutputDuplication>,
        staging: ID3D11Texture2D,
        origin_x: u32,
        origin_y: u32,
        width: u32,
        height: u32,
        /// Top-left of the encoded pixels, in virtual-screen coordinates.
        screen_x: i32,
        screen_y: i32,
    }

    fn open_capture(region: Option<(i32, i32, i32, i32)>, cursor: Option<(i32, i32)>) -> Result<Capture, String> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("dxgi: {e}"))?;
            let mut found: Vec<(IDXGIAdapter1, IDXGIOutput, MonitorRect)> = Vec::new();
            let mut ai = 0u32;
            loop {
                let adapter = match factory.EnumAdapters1(ai) {
                    Ok(a) => a,
                    Err(_) => break,
                };
                let mut oi = 0u32;
                loop {
                    let output = match adapter.EnumOutputs(oi) {
                        Ok(o) => o,
                        Err(_) => break,
                    };
                    if let Ok(desc) = output.GetDesc() {
                        if desc.AttachedToDesktop.as_bool() {
                            let r = desc.DesktopCoordinates;
                            let w = r.right - r.left;
                            let h = r.bottom - r.top;
                            if w > 1 && h > 1 {
                                found.push((
                                    adapter.clone(),
                                    output,
                                    MonitorRect {
                                        x: r.left,
                                        y: r.top,
                                        w,
                                        h,
                                    },
                                ));
                            }
                        }
                    }
                    oi += 1;
                }
                ai += 1;
            }
            let rects: Vec<MonitorRect> = found.iter().map(|(_, _, r)| *r).collect();
            let idx = pick_monitor(&rects, cursor, region).ok_or_else(|| {
                if region.is_some() {
                    "that region is not on a screen".to_string()
                } else {
                    "no monitor".to_string()
                }
            })?;
            let (adapter, output, mon) = &found[idx];
            let frame = frame_on_monitor(*mon, region).ok_or_else(|| "selection too small".to_string())?;
            let adapter_dev: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
            let mut device = None;
            let mut context = None;
            let levels = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0];
            D3D11CreateDevice(
                &adapter_dev,
                D3D_DRIVER_TYPE_UNKNOWN,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("d3d11: {e}"))?;
            let device = device.ok_or("d3d11 device")?;
            let context = context.ok_or("d3d11 context")?;
            let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("output: {e}"))?;
            let dup = output1
                .DuplicateOutput(&device)
                .map_err(|e| format!("desktop capture is busy or unavailable: {e}"))?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: frame.w,
                Height: frame.h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .map_err(|e| format!("staging texture: {e}"))?;
            let staging = staging.ok_or("staging texture")?;
            Ok(Capture {
                device,
                context,
                output: output1,
                dup: Some(dup),
                staging,
                origin_x: frame.x,
                origin_y: frame.y,
                width: frame.w,
                height: frame.h,
                screen_x: mon.x + frame.x as i32,
                screen_y: mon.y + frame.y as i32,
            })
        }
    }

    impl Capture {
        fn latest(&mut self) -> Result<Option<Vec<u8>>, String> {
            let mut newest = None;
            for timeout in [0u32, 0, 0] {
                match self.try_copy(timeout)? {
                    Some(frame) => newest = Some(frame),
                    None => break,
                }
            }
            Ok(newest)
        }

        fn try_copy(&mut self, timeout: u32) -> Result<Option<Vec<u8>>, String> {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            let acquired = unsafe {
                let dup = self.dup.as_ref().ok_or("no duplication")?;
                dup.AcquireNextFrame(timeout, &mut info, &mut resource)
            };
            match acquired {
                Err(e) if same_hr(&e, DXGI_ERROR_WAIT_TIMEOUT) => Ok(None),
                Err(e) if same_hr(&e, DXGI_ERROR_ACCESS_LOST) => {
                    self.dup = None;
                    match unsafe { self.output.DuplicateOutput(&self.device) } {
                        Ok(fresh) => {
                            self.dup = Some(fresh);
                            Ok(None)
                        }
                        Err(err) => {
                            Err(format!("desktop capture is busy or unavailable: {err}"))
                        }
                    }
                }
                Err(e) => Err(format!("capture frame: {e}")),
                Ok(()) => {
                    let copied = unsafe { self.copy_resource(resource) };
                    if let Some(dup) = &self.dup {
                        unsafe {
                            let _ = dup.ReleaseFrame();
                        }
                    }
                    copied.map(Some)
                }
            }
        }

        unsafe fn copy_resource(&self, resource: Option<IDXGIResource>) -> Result<Vec<u8>, String> {
            let resource = resource.ok_or("empty desktop frame")?;
            let tex: ID3D11Texture2D = resource.cast().map_err(|e| e.to_string())?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                return Err(format!("unexpected desktop format {}", desc.Format.0));
            }
            let src: ID3D11Resource = tex.cast().map_err(|e| e.to_string())?;
            let dst: ID3D11Resource = self.staging.cast().map_err(|e| e.to_string())?;
            let crop = D3D11_BOX {
                left: self.origin_x,
                top: self.origin_y,
                front: 0,
                right: self.origin_x + self.width,
                bottom: self.origin_y + self.height,
                back: 1,
            };
            self.context
                .CopySubresourceRegion(&dst, 0, 0, 0, 0, &src, 0, Some(&crop));
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("map frame: {e}"))?;
            let packed = pack_bgra(
                mapped.pData as *const u8,
                mapped.RowPitch as usize,
                self.width as usize,
                self.height as usize,
            );
            self.context.Unmap(&dst, 0);
            packed
        }
    }

    fn capture_first(cap: &mut Capture) -> Result<Vec<u8>, String> {
        let start = Instant::now();
        loop {
            if STOP.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            if let Some(frame) = cap.try_copy(50)? {
                return Ok(frame);
            }
            if start.elapsed() > Duration::from_secs(1) {
                return Err("no desktop frame".into());
            }
        }
    }

    fn pack_bgra(src: *const u8, pitch: usize, w: usize, h: usize) -> Result<Vec<u8>, String> {
        if src.is_null() || pitch < w.saturating_mul(4) {
            return Err("bad frame stride".into());
        }
        let mut out = vec![0u8; w * h * 4];
        unsafe {
            for row in 0..h {
                std::ptr::copy_nonoverlapping(
                    src.add(row * pitch),
                    out.as_mut_ptr().add(row * w * 4),
                    w * 4,
                );
            }
        }
        Ok(out)
    }

    fn spawn_audio(loopback: bool) -> Option<(Arc<AudioFeed>, JoinHandle<()>)> {
        let feed = Arc::new(AudioFeed {
            q: Mutex::new(VecDeque::new()),
            state: Mutex::new(AudioState::Starting),
        });
        let feed2 = feed.clone();
        let handle = std::thread::Builder::new()
            .name(if loopback { "katana-rec-loop" } else { "katana-rec-mic" }.into())
            .spawn(move || audio_thread(loopback, feed2))
            .ok()?;
        Some((feed, handle))
    }

    fn wait_audio(feed: &AudioFeed) -> Result<(), String> {
        let start = Instant::now();
        loop {
            let state = feed.state.lock().unwrap_or_else(|e| e.into_inner());
            match &*state {
                AudioState::Ok => return Ok(()),
                AudioState::Err(e) => return Err(e.clone()),
                AudioState::Starting if start.elapsed() > Duration::from_millis(500) => {
                    return Err("audio device timed out".into());
                }
                AudioState::Starting => drop(state),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn audio_thread(loopback: bool, feed: Arc<AudioFeed>) {
        let label = if loopback { "system audio" } else { "microphone" };
        if let Err(e) = audio_run(loopback, &feed) {
            if let Ok(mut st) = feed.state.lock() {
                if !matches!(*st, AudioState::Ok) {
                    *st = AudioState::Err(format!("{label}: {e}"));
                }
            }
        }
    }

    fn audio_run(loopback: bool, feed: &AudioFeed) -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let flow = if loopback { eRender } else { eCapture };
            let device = enumerator
                .GetDefaultAudioEndpoint(flow, eConsole)
                .map_err(|e| e.to_string())?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
            let fmt = client.GetMixFormat().map_err(|e| e.to_string())?;
            let parsed = parse_mix(fmt);
            let mix = match parsed {
                Ok(m) => m,
                Err(e) => {
                    CoTaskMemFree(Some(fmt.cast()));
                    return Err(e);
                }
            };
            let flags = if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, fmt, None);
            CoTaskMemFree(Some(fmt.cast()));
            init.map_err(|e| e.to_string())?;
            let cap: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
            client.Start().map_err(|e| e.to_string())?;
            if let Ok(mut st) = feed.state.lock() {
                *st = AudioState::Ok;
            }
            let mut prev = None;
            let mut phase = 0.0f64;
            while !STOP.load(Ordering::SeqCst) {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut aflags = 0u32;
                match cap.GetBuffer(&mut data, &mut frames, &mut aflags, None, None) {
                    Ok(()) if frames > 0 => {
                        let mut pcm = Vec::new();
                        if aflags & 2 != 0 || data.is_null() {
                            let n = frames as usize;
                            let silent = vec![(0.0f32, 0.0f32); n];
                            resample_to_48k(&mut prev, &mut phase, &silent, mix.rate, &mut pcm);
                        } else {
                            let bytes = frames as usize * mix.block as usize;
                            let slice = std::slice::from_raw_parts(data, bytes);
                            let frames_f = decode_frames(slice, frames, &mix);
                            resample_to_48k(&mut prev, &mut phase, &frames_f, mix.rate, &mut pcm);
                        }
                        let _ = cap.ReleaseBuffer(frames);
                        push_i16(&feed.q, &pcm);
                    }
                    Ok(()) => std::thread::sleep(Duration::from_millis(8)),
                    Err(_) => std::thread::sleep(Duration::from_millis(8)),
                }
            }
            let _ = client.Stop();
            Ok(())
        }
    }

    struct MixFmt {
        channels: u16,
        rate: u32,
        bits: u16,
        float: bool,
        block: u16,
    }

    unsafe fn parse_mix(fmt: *mut WAVEFORMATEX) -> Result<MixFmt, String> {
        if fmt.is_null() {
            return Err("no mix format".into());
        }
        // WAVEFORMATEX is packed, so field reads go through unaligned loads.
        let p = fmt as *const u8;
        let u16_at = |off| std::ptr::read_unaligned(p.add(off) as *const u16);
        let u32_at = |off| std::ptr::read_unaligned(p.add(off) as *const u32);
        let mut tag = u16_at(0);
        let channels = u16_at(2);
        let rate = u32_at(4);
        let align = u16_at(12);
        let bits = u16_at(14);
        let cb = u16_at(16);
        let mut is_float = tag == 3;
        if tag == 0xFFFE {
            if cb < 22 {
                return Err("bad extensible format".into());
            }
            let d1 = u32_at(24);
            tag = d1 as u16;
            is_float = d1 == 3;
        }
        let ok = (is_float && bits == 32) || (tag == 1 && bits == 16);
        if !ok {
            return Err(format!("unsupported audio format tag {tag} bits {bits}"));
        }
        if channels == 0 || channels > 2 {
            return Err(format!("unsupported channel count {channels}"));
        }
        if rate < 8_000 || rate > 192_000 {
            return Err("unsupported sample rate".into());
        }
        let block = if align == 0 { channels * (bits / 8) } else { align };
        Ok(MixFmt {
            channels,
            rate,
            bits,
            float: is_float,
            block,
        })
    }

    fn decode_frames(data: &[u8], frames: u32, mix: &MixFmt) -> Vec<(f32, f32)> {
        let bps = (mix.bits / 8) as usize;
        let ch = mix.channels as usize;
        let mut out = Vec::with_capacity(frames as usize);
        for i in 0..frames as usize {
            let base = i * mix.block as usize;
            if base + ch * bps > data.len() {
                break;
            }
            let sample = |c: usize| -> f32 {
                let o = base + c * bps;
                if mix.float {
                    f32::from_le_bytes(data[o..o + 4].try_into().unwrap_or([0; 4]))
                } else {
                    i16::from_le_bytes(data[o..o + 2].try_into().unwrap_or([0; 2])) as f32 / 32768.0
                }
            };
            let l = sample(0);
            let r = if ch > 1 { sample(1) } else { l };
            out.push((l, r));
        }
        out
    }

    fn push_i16(q: &Mutex<VecDeque<i16>>, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let mut g = q.lock().unwrap_or_else(|e| e.into_inner());
        g.extend(samples);
        let cap = AUDIO_RATE as usize * 2 * 3;
        if g.len() > cap {
            let drop_n = g.len() - cap;
            g.drain(0..drop_n);
        }
    }

    fn pull_i16(q: &Mutex<VecDeque<i16>>, n: usize) -> Vec<i16> {
        let mut g = q.lock().unwrap_or_else(|e| e.into_inner());
        let n = n.min(g.len()) & !1;
        g.drain(0..n).collect()
    }

    fn pull_mix(feeds: &[Arc<AudioFeed>], need: usize) -> Vec<i16> {
        if feeds.is_empty() || need == 0 {
            return Vec::new();
        }
        if feeds.len() == 1 {
            let mut v = pull_i16(&feeds[0].q, need);
            v.resize(need, 0);
            return v;
        }
        let a = pull_i16(&feeds[0].q, need);
        let b = pull_i16(&feeds[1].q, need);
        mix_i16(&a, &b, need)
    }
}

#[cfg(windows)]
pub use win::{request_stop, start};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_crop_is_even_and_inside_the_monitor() {
        let mon = MonitorRect { x: 0, y: 0, w: 1920, h: 1080 };
        let full = frame_on_monitor(mon, None).unwrap();
        assert_eq!((full.x, full.y, full.w, full.h), (0, 0, 1920, 1080));
        let odd = frame_on_monitor(MonitorRect { x: 0, y: 0, w: 1921, h: 1081 }, None).unwrap();
        assert_eq!((odd.w, odd.h), (1920, 1080));
        let crop = frame_on_monitor(mon, Some((10, 20, 101, 51))).unwrap();
        assert_eq!((crop.x, crop.y, crop.w, crop.h), (10, 20, 100, 50));
        assert!(frame_on_monitor(mon, Some((1919, 0, 10, 10))).is_none());
        assert!(frame_on_monitor(mon, Some((5000, 5000, 40, 40))).is_none());
    }

    #[test]
    fn pick_monitor_uses_overlap_or_cursor() {
        let mons = [
            MonitorRect { x: 0, y: 0, w: 1920, h: 1080 },
            MonitorRect { x: 1920, y: 0, w: 1280, h: 720 },
        ];
        assert_eq!(pick_monitor(&mons, Some((2000, 10)), None), Some(1));
        assert_eq!(pick_monitor(&mons, Some((10, 10)), None), Some(0));
        assert_eq!(pick_monitor(&mons, None, Some((1000, 0, 200, 100))), Some(0));
        assert_eq!(pick_monitor(&mons, None, Some((1900, 0, 400, 100))), Some(1));
        assert_eq!(pick_monitor(&mons, None, Some((9000, 0, 10, 10))), None);
    }

    #[test]
    fn bitrate_and_level_follow_the_frame() {
        assert_eq!(h264_bitrate(1920, 1080, 30), 7_776_000);
        assert_eq!(h264_bitrate(64, 64, 1), 400_000);
        assert_eq!(h264_level_idc(1280, 720), 31);
        assert_eq!(h264_level_idc(1920, 1080), 40);
        assert_eq!(h264_level_idc(3840, 2160), 51);
    }

    #[test]
    fn nv12_white_is_limited_range() {
        let bgra = vec![255u8; 2 * 2 * 4];
        let nv = bgra_to_nv12(2, 2, &bgra).unwrap();
        assert_eq!(nv.len(), 6);
        assert_eq!(&nv[..4], &[235, 235, 235, 235]);
        assert_eq!(&nv[4..], &[128, 128]);
        assert!(bgra_to_nv12(3, 2, &bgra).is_err());
        assert!(bgra_to_nv12(2, 2, &[0u8; 4]).is_err());
    }

    #[test]
    fn mix_clamps_and_resample_keeps_48k() {
        let mixed = mix_i16(&[30_000, -30_000], &[10_000, -10_000], 2);
        assert_eq!(mixed[0], i16::MAX);
        assert_eq!(mixed[1], i16::MIN);
        let mut prev = None;
        let mut phase = 0.0;
        let mut out = Vec::new();
        resample_to_48k(&mut prev, &mut phase, &[(0.5, -0.5)], 48_000, &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 16383).abs() <= 1);
        assert!((out[1] + 16383).abs() <= 1);
        let mut prev = None;
        let mut phase = 0.0;
        let mut out = Vec::new();
        let frames = vec![(0.0, 0.0); 441];
        resample_to_48k(&mut prev, &mut phase, &frames, 44_100, &mut out);
        let samples = out.len() / 2;
        assert!((470..=490).contains(&samples), "{samples}");
    }

    #[test]
    fn outline_sits_outside_a_region_and_on_a_full_monitor_edge() {
        let screen = (0, 0, 1920, 1080);
        let region = outline_geometry(100, 80, 400, 300, screen.0, screen.1, screen.2, screen.3);
        assert_eq!(region.x, 100 - (OUTLINE_RED + OUTLINE_DIM));
        assert_eq!(region.y, 80 - (OUTLINE_RED + OUTLINE_DIM));
        assert_eq!(region.hole_x, OUTLINE_RED + OUTLINE_DIM);
        assert_eq!(region.hole_y, OUTLINE_RED + OUTLINE_DIM);
        assert_eq!((region.hole_w, region.hole_h), (400, 300));
        // One pixel outside the hole is red; the hole itself is clear; further out is dim.
        assert_eq!(
            outline_pixel(region.hole_x - 1, region.hole_y, region.hole_x, region.hole_y, 400, 300),
            [0x48, 0x1D, 0xE1, 0xFF]
        );
        assert_eq!(
            outline_pixel(region.hole_x, region.hole_y, region.hole_x, region.hole_y, 400, 300),
            [0, 0, 0, 0]
        );
        assert_eq!(
            outline_pixel(region.hole_x - 4, region.hole_y + 20, region.hole_x, region.hole_y, 400, 300)[3],
            64
        );

        let full = outline_geometry(0, 0, 1920, 1080, screen.0, screen.1, screen.2, screen.3);
        assert_eq!((full.x, full.y, full.w, full.h), (0, 0, 1920, 1080));
        assert_eq!((full.hole_x, full.hole_y), (OUTLINE_RED, OUTLINE_RED));
        assert_eq!(outline_pixel(0, 10, full.hole_x, full.hole_y, full.hole_w, full.hole_h)[3], 0xFF);
        assert_eq!(outline_pixel(2, 2, full.hole_x, full.hole_y, full.hole_w, full.hole_h), [0, 0, 0, 0]);
    }
}
