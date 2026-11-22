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
    // SAFETY: the pseudo-handle from GetCurrentProcess is always valid for the current process.
    unsafe { K32EmptyWorkingSet(GetCurrentProcess()) };
}
#[cfg(not(windows))]
pub fn empty_working_set() {}

/// Non-client messages the main window must hand straight to `DefWindowProcW`, bypassing GPUI: a left press or
/// double-click on the caption (`HTCAPTION` = 2, which GPUI answers for `WindowControlArea::Drag`) and a right
/// click on it (system menu). GPUI 0.2.2 turns `WM_NCLBUTTONDOWN` into a `MouseDown` and swallows the message
/// when the event was "handled"; the shell root `track_focus`es, and GPUI's focus listener `prevent_default`s
/// every mouse down, so the press never reached `DefWindowProcW` and the title bar could not move the window
/// (nor snap, drag-restore or double-click maximize). GPUI has no Windows `start_window_move` either.
#[cfg(any(windows, test))]
pub fn caption_passthrough(msg: u32, hit: usize) -> bool {
    const HTCAPTION: usize = 2;
    // WM_NCLBUTTONDOWN, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONDOWN, WM_NCRBUTTONUP
    hit == HTCAPTION && matches!(msg, 0x00A1 | 0x00A3 | 0x00A4 | 0x00A5)
}

/// The caption buttons, same cause: GPUI only arms its own min/max/close handling when the `MouseDown` it made of
/// `WM_NCLBUTTONDOWN` was not handled, which the root focus listener always does. Returns the `WM_SYSCOMMAND`
/// for a press + release on the same button (`HTMINBUTTON` 8, `HTMAXBUTTON` 9, `HTCLOSE` 20).
#[cfg(any(windows, test))]
pub fn caption_button_command(hit: usize, maximized: bool) -> Option<usize> {
    match hit {
        8 => Some(0xF020),                                     // SC_MINIMIZE
        9 => Some(if maximized { 0xF120 } else { 0xF030 }), // SC_RESTORE / SC_MAXIMIZE
        20 => Some(0xF060),                                    // SC_CLOSE
        _ => None,
    }
}

/// Minimize/restore hook (Windows). On other platforms `install` reports `false` and
/// the app simply never suspends (macOS gets its own hook in a later phase).
#[cfg(windows)]
pub mod size_hook {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    type SubProc = unsafe extern "system" fn(Hwnd, u32, usize, isize, usize, usize) -> isize;
    type MinimizeCallback = Box<dyn Fn(bool)>;
    thread_local! {
        static HOOK: RefCell<Option<MinimizeCallback>> = const { RefCell::new(None) };
        static MIN: Cell<bool> = const { Cell::new(false) };
        /// caption button under the last WM_NCLBUTTONDOWN (0 = none)
        static NC_PRESSED: Cell<usize> = const { Cell::new(0) };
    }
    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn SetWindowSubclass(h: Hwnd, f: SubProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn DefWindowProcW(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
        fn PostMessageW(h: Hwnd, m: u32, w: usize, l: isize) -> i32;
        fn IsZoomed(h: Hwnd) -> i32;
    }
    unsafe extern "system" fn sub(h: Hwnd, msg: u32, w: usize, l: isize, _id: usize, _d: usize) -> isize {
        if super::caption_passthrough(msg, w) {
            // native caption drag (modal move loop: Aero Snap, drag-restore from maximized, per-monitor DPI via
            // WM_DPICHANGED, which GPUI handles), caption double-click maximize/restore, system menu
            // SAFETY: forwarding the unmodified message to the default window procedure for the live window `h`.
            return unsafe { DefWindowProcW(h, msg, w, l) };
        }
        match msg {
            // WM_NCLBUTTONDOWN on a caption button: remember it (GPUI would drop it, see caption_button_command)
            0x00A1 if super::caption_button_command(w, false).is_some() => {
                NC_PRESSED.with(|p| p.set(w));
                return 0;
            }
            // WM_NCLBUTTONUP: act like a native button (release over the button that was pressed)
            0x00A2 if NC_PRESSED.with(|p| p.get()) != 0 => {
                let pressed = NC_PRESSED.with(|p| p.replace(0));
                // SAFETY: `h` is the live window handle this window procedure was invoked for.
                if pressed == w && let Some(cmd) = super::caption_button_command(w, unsafe { IsZoomed(h) } != 0) {
                    // SAFETY: posting WM_SYSCOMMAND to our own live window; no pointers are passed.
                    unsafe { PostMessageW(h, 0x0112, cmd, 0) }; // WM_SYSCOMMAND
                }
                return 0;
            }
            _ => {}
        }
        if msg == 0x0005 {
            // WM_SIZE
            if let Some(now_min) = super::size_kind(w)
                && MIN.with(|m| m.replace(now_min)) != now_min
            {
                HOOK.with(|c| {
                    if let Some(f) = c.borrow().as_ref() {
                        f(now_min)
                    }
                });
            }
        }
        // SAFETY: forwarding the unmodified message to the next subclass procedure, as the subclass contract requires.
        unsafe { DefSubclassProc(h, msg, w, l) }
    }
    /// Must be called on the UI thread with the window's HWND.
    pub fn install(hwnd: isize, f: MinimizeCallback) -> bool {
        HOOK.with(|c| *c.borrow_mut() = Some(f));
        // SAFETY: `hwnd` is the live top-level window; `sub` is a valid extern "system" subclass procedure for the window's lifetime.
        unsafe { SetWindowSubclass(hwnd as Hwnd, sub, 1, 0) != 0 }
    }
}

#[cfg(all(windows, not(test)))]
pub fn hwnd_of(window: &gpui::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// No native handle: other platforms, and GPUI test windows (`TestWindow::window_handle` is `unimplemented!`).
#[cfg(any(not(windows), test))]
pub fn hwnd_of(_: &gpui::Window) -> Option<isize> {
    None
}




/// Work area (taskbar excluded, physical px: x, y, w, h) of the monitor `hwnd` is on.
#[cfg(windows)]
pub fn work_area_of(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    use std::ffi::c_void;
    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: [i32; 4],
        work: [i32; 4],
        flags: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MonitorFromWindow(h: *mut c_void, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(m: *mut c_void, i: *mut MonitorInfo) -> i32;
    }
    // SAFETY: the monitor handle comes from MonitorFromWindow for a live window; MonitorInfo is a local with cbSize set before the call.
    unsafe {
        let m = MonitorFromWindow(hwnd as *mut c_void, 2);
        let mut i = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: [0; 4], work: [0; 4], flags: 0 };
        if m.is_null() || GetMonitorInfoW(m, &mut i) == 0 {
            return None;
        }
        Some((i.work[0], i.work[1], i.work[2] - i.work[0], i.work[3] - i.work[1]))
    }
}
#[cfg(not(windows))]
pub fn work_area_of(_: isize) -> Option<(i32, i32, i32, i32)> {
    None
}

/// Show a window without activating it: `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, topmost, `SWP_NOACTIVATE`,
/// `SW_SHOWNOACTIVATE` (the track-change card must never take the focus from the app the user is working in).
#[cfg(windows)]
pub fn show_no_activate(hwnd: isize, x: i32, y: i32, w: i32, ht: i32) {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowLongPtrW(h: *mut c_void, i: i32) -> isize;
        fn SetWindowLongPtrW(h: *mut c_void, i: i32, v: isize) -> isize;
        fn SetWindowPos(h: *mut c_void, after: *mut c_void, x: i32, y: i32, w: i32, h2: i32, flags: u32) -> i32;
        #[link_name = "GetWindowRect"]
        fn GetWindowRect2(h: *mut c_void, r: *mut Rect) -> i32;
        #[link_name = "GetClientRect"]
        fn GetClientRect2(h: *mut c_void, r: *mut Rect) -> i32;
        fn ShowWindow(h: *mut c_void, cmd: i32) -> i32;
    }
    // SAFETY: `h` is this process's live top-level window handle; ShowWindow has no other preconditions.
    unsafe {
        let hw = hwnd as *mut c_void;
        let ex = GetWindowLongPtrW(hw, -20);
        SetWindowLongPtrW(hw, -20, ex | 0x0800_0000 | 0x80 | 0x8);
        SetWindowLongPtrW(hw, -16, 0x8000_0000u32 as i32 as isize); // WS_POPUP: no caption/frame, client = the whole window
        // HWND_TOPMOST = -1; SWP_NOACTIVATE | SWP_FRAMECHANGED
        SetWindowPos(hw, -1isize as *mut c_void, x, y, w, ht, 0x0010 | 0x0020);
        // whatever non-client frame the window still has: grow the outer size so the CLIENT area is exactly w x ht
        let (mut wr, mut cr) = (Rect { l: 0, t: 0, r: 0, b: 0 }, Rect { l: 0, t: 0, r: 0, b: 0 });
        GetWindowRect2(hw, &mut wr);
        GetClientRect2(hw, &mut cr);
        let (dw, dh) = ((wr.r - wr.l) - (cr.r - cr.l), (wr.b - wr.t) - (cr.b - cr.t));
        if dw > 0 || dh > 0 {
            SetWindowPos(hw, -1isize as *mut c_void, x, y, w + dw, ht + dh, 0x0010);
        }
        ShowWindow(hw, 4);
    }
}
#[cfg(not(windows))]
pub fn show_no_activate(_: isize, _: i32, _: i32, _: i32, _: i32) {}

/// Restore a minimized window and bring it to the foreground (card click -> main window).
#[cfg(windows)]
pub fn restore_foreground(hwnd: isize) {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn ShowWindow(h: *mut c_void, cmd: i32) -> i32;
        fn IsIconic(h: *mut c_void) -> i32;
        fn SetForegroundWindow(h: *mut c_void) -> i32;
    }
    // SAFETY: `h` is this process's live top-level window handle; these user32 calls only take the handle and plain integers.
    unsafe {
        let h = hwnd as *mut c_void;
        if IsIconic(h) != 0 {
            ShowWindow(h, 9);
        }
        SetForegroundWindow(h);
    }
}
#[cfg(not(windows))]
pub fn restore_foreground(_: isize) {}


/// Win32 `RECT` (shared by every raw declaration in this file).
#[cfg(windows)]
#[repr(C)]
struct Rect {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

/// `SONIX_NO_ACTIVATE=1` (test runs while someone uses the PC): show the window without activating it and park it
/// far off-screen (it still paints, `PrintWindow` still works).
#[cfg(windows)]
pub fn show_parked_no_activate(hwnd: isize) {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowPos(h: *mut c_void, after: *mut c_void, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn ShowWindow(h: *mut c_void, cmd: i32) -> i32;
    }
    // SAFETY: `h` is this process's live top-level window handle; ShowWindow has no other preconditions.
    unsafe {
        // SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
        SetWindowPos(hwnd as *mut c_void, std::ptr::null_mut(), -30000, -30000, 0, 0, 0x0001 | 0x0004 | 0x0010);
        ShowWindow(hwnd as *mut c_void, 4); // SW_SHOWNOACTIVATE
    }
}
#[cfg(not(windows))]
pub fn show_parked_no_activate(_: isize) {}

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
    #[test]
    fn caption_messages_bypass_gpui() {
        assert!(caption_passthrough(0x00A1, 2)); // WM_NCLBUTTONDOWN on the caption
        assert!(caption_passthrough(0x00A3, 2)); // double-click
        assert!(!caption_passthrough(0x00A1, 8)); // HTMINBUTTON stays with GPUI
        assert!(!caption_passthrough(0x00A1, 20)); // HTCLOSE
        assert!(!caption_passthrough(0x0201, 2)); // client WM_LBUTTONDOWN
        assert_eq!(caption_button_command(8, false), Some(0xF020));
        assert_eq!(caption_button_command(9, false), Some(0xF030));
        assert_eq!(caption_button_command(9, true), Some(0xF120));
        assert_eq!(caption_button_command(20, true), Some(0xF060));
        assert_eq!(caption_button_command(2, false), None);
    }
}
