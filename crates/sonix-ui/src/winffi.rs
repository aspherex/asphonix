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
                if pressed == w && let Some(cmd) = super::caption_button_command(w, unsafe { IsZoomed(h) } != 0) {
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
        unsafe { DefSubclassProc(h, msg, w, l) }
    }
    /// Must be called on the UI thread with the window's HWND.
    pub fn install(hwnd: isize, f: MinimizeCallback) -> bool {
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

/// Parity harness only: grab the window's client area as top-down BGRA (`PrintWindow` with
/// `PW_CLIENTONLY | PW_RENDERFULLCONTENT`, which works for DirectX/DComp windows and does not
/// depend on the window being on top). Returns `(width, height, bgra)` in physical pixels.
#[cfg(all(windows, feature = "parity"))]
pub fn capture_client(hwnd: isize) -> Option<(u32, u32, Vec<u8>)> {
    use std::ffi::c_void;
    #[repr(C)]
    struct BmiHeader {
        size: u32,
        w: i32,
        h: i32,
        planes: u16,
        bpp: u16,
        compression: u32,
        size_image: u32,
        xppm: i32,
        yppm: i32,
        used: u32,
        important: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetClientRect(h: *mut c_void, r: *mut Rect) -> i32;
        fn PrintWindow(h: *mut c_void, dc: *mut c_void, flags: u32) -> i32;
        fn GetDC(h: *mut c_void) -> *mut c_void;
        fn ReleaseDC(h: *mut c_void, dc: *mut c_void) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: *mut c_void) -> *mut c_void;
        fn CreateCompatibleBitmap(dc: *mut c_void, w: i32, h: i32) -> *mut c_void;
        fn SelectObject(dc: *mut c_void, o: *mut c_void) -> *mut c_void;
        fn GetDIBits(dc: *mut c_void, bmp: *mut c_void, start: u32, lines: u32, bits: *mut c_void, bmi: *mut BmiHeader, usage: u32) -> i32;
        fn DeleteObject(o: *mut c_void) -> i32;
        fn DeleteDC(dc: *mut c_void) -> i32;
    }
    unsafe {
        let h = hwnd as *mut c_void;
        let mut r = Rect { l: 0, t: 0, r: 0, b: 0 };
        if GetClientRect(h, &mut r) == 0 {
            return None;
        }
        let (w, ht) = ((r.r - r.l) as u32, (r.b - r.t) as u32);
        if w == 0 || ht == 0 {
            return None;
        }
        let screen = GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w as i32, ht as i32);
        let old = SelectObject(dc, bmp);
        let ok = PrintWindow(h, dc, 1 | 2) != 0;
        let mut out = vec![0u8; (w * ht * 4) as usize];
        let mut bmi = BmiHeader { size: std::mem::size_of::<BmiHeader>() as u32, w: w as i32, h: -(ht as i32), planes: 1, bpp: 32, compression: 0, size_image: 0, xppm: 0, yppm: 0, used: 0, important: 0 };
        let lines = GetDIBits(dc, bmp, 0, ht, out.as_mut_ptr() as *mut c_void, &mut bmi, 0);
        SelectObject(dc, old);
        DeleteObject(bmp);
        DeleteDC(dc);
        ReleaseDC(std::ptr::null_mut(), screen);
        if !ok || lines == 0 {
            return None;
        }
        Some((w, ht, out))
    }
}

/// Parity harness only: deliver a mouse message to our own window through `SendMessageW` (a
/// direct, synchronous call of GPUI's window procedure; it must NOT run inside an app update).
/// Coordinates are physical client pixels. Held modifiers are faked for the duration of the call
/// with `SetKeyboardState` (this thread's key table, which is what `GetKeyState` reads), so
/// Ctrl/Shift/Alt clicks work without touching the real keyboard.
#[cfg(all(windows, feature = "parity"))]
pub fn send_mouse(hwnd: isize, kind: &str, x: i32, y: i32, button: &str, mods: (bool, bool, bool), wheel_notches: f32) {
    let (ctrl, shift, alt) = mods;
    use std::ffi::c_void;
    #[repr(C)]
    struct Pt {
        x: i32,
        y: i32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendMessageW(h: *mut c_void, msg: u32, w: usize, l: isize) -> isize;
        fn GetKeyboardState(keys: *mut u8) -> i32;
        fn SetKeyboardState(keys: *const u8) -> i32;
        fn ClientToScreen(h: *mut c_void, p: *mut Pt) -> i32;
    }
    let h = hwnd as *mut c_void;
    let lp = ((y as isize & 0xffff) << 16) | (x as isize & 0xffff);
    let mk = |down: bool| -> usize {
        let mut m = 0usize;
        if down {
            m |= match button {
                "right" => 2,
                "middle" => 0x10,
                _ => 1,
            };
        }
        if shift {
            m |= 4;
        }
        if ctrl {
            m |= 8;
        }
        m
    };
    let (down_msg, up_msg) = match button {
        "right" => (0x204u32, 0x205u32),
        "middle" => (0x207, 0x208),
        _ => (0x201, 0x202),
    };
    unsafe {
        let mut saved = [0u8; 256];
        GetKeyboardState(saved.as_mut_ptr());
        let mut keys = saved;
        let set = |k: &mut [u8; 256], vk: usize, on: bool| {
            k[vk] = if on { 0x80 } else { 0 };
        };
        set(&mut keys, 0x11, ctrl);
        set(&mut keys, 0xA2, ctrl);
        set(&mut keys, 0x10, shift);
        set(&mut keys, 0xA0, shift);
        set(&mut keys, 0x12, alt);
        set(&mut keys, 0xA4, alt);
        SetKeyboardState(keys.as_ptr());
        match kind {
            "move" => {
                SendMessageW(h, 0x200, mk(false), lp);
            }
            "dragmove" => {
                SendMessageW(h, 0x200, mk(true), lp);
            }
            "down" => {
                SendMessageW(h, down_msg, mk(true), lp);
            }
            "up" => {
                SendMessageW(h, up_msg, mk(false), lp);
            }
            "wheel" => {
                let mut p = Pt { x, y };
                ClientToScreen(h, &mut p);
                let delta = (wheel_notches * 120.0) as i16 as u16 as usize;
                let slp = ((p.y as isize & 0xffff) << 16) | (p.x as isize & 0xffff);
                SendMessageW(h, 0x20A, (delta << 16) | mk(false), slp);
            }
            _ => {}
        }
        SetKeyboardState(saved.as_ptr());
    }
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

/// Private working set of this process in bytes (diagnostics, `$SONIX_LOG`).
#[cfg(windows)]
pub fn private_ws() -> u64 {
    #[repr(C)]
    struct Pmc {
        cb: u32,
        page_faults: u32,
        peak_ws: usize,
        ws: usize,
        quota: [usize; 4],
        pagefile: usize,
        peak_pagefile: usize,
        private_usage: usize,
        private_ws: usize,
        shared_commit: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(p: *mut std::ffi::c_void, c: *mut Pmc, cb: u32) -> i32;
    }
    let mut m = Pmc { cb: std::mem::size_of::<Pmc>() as u32, page_faults: 0, peak_ws: 0, ws: 0, quota: [0; 4], pagefile: 0, peak_pagefile: 0, private_usage: 0, private_ws: 0, shared_commit: 0 };
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) };
    m.private_ws as u64
}
#[cfg(not(windows))]
pub fn private_ws() -> u64 {
    0
}
