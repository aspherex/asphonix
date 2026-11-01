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
    type MinimizeCallback = Box<dyn Fn(bool)>;
    thread_local! {
        static HOOK: RefCell<Option<MinimizeCallback>> = const { RefCell::new(None) };
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
    struct Rect {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }
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
