//! Tiny raw Win32 FFI (no extra crates beyond `raw-window-handle`, already a GPUI
//! dependency): minimize/restore notification without polling, and working-set trim.
//!
//! GPUI 0.2.2 swallows `SIZE_MINIMIZED` inside its platform layer and exposes no
//! callback for it, so we subclass our own HWND and watch `WM_SIZE` (see
//! docs/GPUI-GAPS.md). The HWND comes from `HasWindowHandle for Window`, not from a
//! window-title lookup.

/// Pure decoding of a `WM_SIZE` `wParam`: `Some(true)` = minimized, `Some(false)` =
/// restored/maximized, `None` for sizes that carry no change (SIZE_MAXHIDE/SHOW).
pub fn size_kind(wparam: usize) -> Option<bool> {
    match wparam {
        1 => Some(true),      // SIZE_MINIMIZED
        0 | 2 => Some(false), // SIZE_RESTORED, SIZE_MAXIMIZED
        _ => None,
    }
}

#[cfg(windows)]
pub fn empty_working_set() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32EmptyWorkingSet(p: *mut std::ffi::c_void) -> i32;
    }
    unsafe { K32EmptyWorkingSet(GetCurrentProcess()) };
}
#[cfg(not(windows))]
pub fn empty_working_set() {}

/// Minimize/restore hook (Windows). On other platforms `install` reports `false` and
/// the app simply never suspends (macOS gets its own hook in a later phase).
#[cfg(windows)]
pub mod size_hook {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    type SubProc = unsafe extern "system" fn(Hwnd, u32, usize, isize, usize, usize) -> isize;
    thread_local! {
        static HOOK: RefCell<Option<Box<dyn Fn(bool)>>> = const { RefCell::new(None) };
        static MIN: Cell<bool> = const { Cell::new(false) };
    }
    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn SetWindowSubclass(h: Hwnd, f: SubProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
    }
    unsafe extern "system" fn sub(h: Hwnd, msg: u32, w: usize, l: isize, _id: usize, _d: usize) -> isize {
        if msg == 0x0005 {
            // WM_SIZE
            if let Some(now_min) = super::size_kind(w) {
                if MIN.with(|m| m.replace(now_min)) != now_min {
                    HOOK.with(|c| {
                        if let Some(f) = c.borrow().as_ref() {
                            f(now_min)
                        }
                    });
                }
            }
        }
        unsafe { DefSubclassProc(h, msg, w, l) }
    }
    /// Must be called on the UI thread with the window's HWND.
    pub fn install(hwnd: isize, f: Box<dyn Fn(bool)>) -> bool {
        HOOK.with(|c| *c.borrow_mut() = Some(f));
        unsafe { SetWindowSubclass(hwnd as Hwnd, sub, 1, 0) != 0 }
    }
}

#[cfg(windows)]
pub fn hwnd_of(window: &gpui::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wm_size_decoding() {
        assert_eq!(size_kind(1), Some(true));
        assert_eq!(size_kind(0), Some(false));
        assert_eq!(size_kind(2), Some(false));
        assert_eq!(size_kind(3), None);
        assert_eq!(size_kind(4), None);
    }
}
