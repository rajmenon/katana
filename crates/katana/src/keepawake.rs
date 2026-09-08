//! Session toggle: keep display + system from sleeping.

use std::sync::atomic::{AtomicBool, Ordering};

pub const ES_CONTINUOUS: u32 = 0x8000_0000;
pub const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
pub const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

static ON: AtomicBool = AtomicBool::new(false);

pub fn is_on() -> bool {
    ON.load(Ordering::SeqCst)
}

pub fn execution_state(on: bool) -> u32 {
    if on {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
    } else {
        ES_CONTINUOUS
    }
}

pub fn set(on: bool) -> bool {
    ON.store(on, Ordering::SeqCst);
    apply(on);
    on
}

pub fn toggle() -> bool {
    let on = !is_on();
    set(on)
}

fn apply(on: bool) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        type SetFn = unsafe extern "system" fn(u32) -> u32;
        let flags = execution_state(on);
        if let Ok(k) = GetModuleHandleW(windows::core::w!("kernel32.dll")) {
            if let Some(p) = GetProcAddress(k, windows::core::s!("SetThreadExecutionState")) {
                let f: SetFn = std::mem::transmute(p);
                let _ = f(flags);
            }
        }
    }
    let _ = on;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_include_display_when_on() {
        let on = execution_state(true);
        assert_eq!(on & ES_DISPLAY_REQUIRED, ES_DISPLAY_REQUIRED);
        assert_eq!(on & ES_SYSTEM_REQUIRED, ES_SYSTEM_REQUIRED);
        assert_eq!(on & ES_CONTINUOUS, ES_CONTINUOUS);
        assert_eq!(execution_state(false), ES_CONTINUOUS);
    }

    #[test]
    fn toggle_flips_session_flag() {
        let start = is_on();
        let a = toggle();
        assert_ne!(a, start);
        let b = toggle();
        assert_eq!(b, start);
        set(start);
    }
}
