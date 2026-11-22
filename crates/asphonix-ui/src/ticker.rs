//! A precise one-shot delay for the live layer's frame pacing (`crate::live`). GPUI's `Timer` on Windows is a WinRT
//! thread-pool timer with ~15.6 ms granularity (a 20 ms wait became 31 ms). Here one small thread waits on a
//! high-resolution waitable timer (`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`) or a wake-up event, keeps the pending
//! requests ordered by due time, drops cancelled ones (the waiting task was dropped) so they never delay an earlier
//! one, and wakes the UI task through a oneshot channel. Without the thread (spawn failed, not Windows) it falls back
//! to `gpui::Timer`.
use futures::channel::oneshot;
use std::time::{Duration, Instant};

type Req = (Instant, oneshot::Sender<()>);

/// Resolves after `d` (to within about a millisecond on Windows).
pub fn delay(d: Duration) -> impl std::future::Future<Output = ()> {
    use futures::future::Either;
    let (done, wait) = oneshot::channel();
    if imp::send((Instant::now() + d, done)) {
        Either::Left(async move {
            let _ = wait.await;
        })
    } else {
        Either::Right(async move {
            gpui::Timer::after(d).await;
        })
    }
}

/// Fire the due requests, drop the cancelled ones; returns the earliest remaining due time.
fn service(q: &mut Vec<Req>) -> Option<Instant> {
    q.retain(|(_, done)| !done.is_canceled());
    let now = Instant::now();
    let mut i = 0;
    while i < q.len() {
        if q[i].0 <= now {
            let (_, done) = q.swap_remove(i);
            let _ = done.send(());
        } else {
            i += 1;
        }
    }
    q.iter().map(|r| r.0).min()
}

#[cfg(windows)]
mod imp {
    use super::{Req, service};
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Instant;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateWaitableTimerExW(attr: *const c_void, name: *const u16, flags: u32, access: u32) -> *mut c_void;
        fn SetWaitableTimer(h: *mut c_void, due: *const i64, period: i32, f: *const c_void, arg: *const c_void, resume: i32) -> i32;
        fn CreateEventW(attr: *const c_void, manual: i32, initial: i32, name: *const u16) -> *mut c_void;
        fn SetEvent(h: *mut c_void) -> i32;
        fn WaitForMultipleObjects(n: u32, h: *const *mut c_void, all: i32, ms: u32) -> u32;
    }
    const HIGH_RESOLUTION: u32 = 0x2;
    const TIMER_ALL_ACCESS: u32 = 0x001F_0003;
    const INFINITE: u32 = u32::MAX;

    struct Ticker {
        tx: Sender<Req>,
        /// auto-reset event: a new request arrived
        wake: isize,
    }
    static TICKER: OnceLock<Option<Ticker>> = OnceLock::new();

    pub fn send(r: Req) -> bool {
        let Some(t) = TICKER.get_or_init(start) else { return false };
        if t.tx.send(r).is_err() {
            return false;
        }
        unsafe { SetEvent(t.wake as *mut c_void) };
        true
    }

    fn start() -> Option<Ticker> {
        let (tx, rx) = channel::<Req>();
        let (wake, timer) = unsafe {
            let mut timer = CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), HIGH_RESOLUTION, TIMER_ALL_ACCESS);
            if timer.is_null() {
                timer = CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), 0, TIMER_ALL_ACCESS);
            }
            (CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()), timer)
        };
        if wake.is_null() || timer.is_null() {
            return None;
        }
        let (wake, timer) = (wake as isize, timer as isize);
        std::thread::Builder::new().name("live-ticker".into()).spawn(move || run(rx, wake, timer)).ok()?;
        Some(Ticker { tx, wake })
    }

    fn run(rx: Receiver<Req>, wake: isize, timer: isize) {
        let mut q: Vec<Req> = Vec::new();
        loop {
            loop {
                match rx.try_recv() {
                    Ok(r) => q.push(r),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            }
            let handles = [wake as *mut c_void, timer as *mut c_void];
            match service(&mut q) {
                None => unsafe {
                    WaitForMultipleObjects(1, handles.as_ptr(), 0, INFINITE);
                },
                Some(due) => unsafe {
                    // relative due time in 100 ns units (negative = relative)
                    let left = due.saturating_duration_since(Instant::now());
                    let rel = -((left.as_nanos() / 100).max(1) as i64);
                    SetWaitableTimer(timer as *mut c_void, &rel, 0, std::ptr::null(), std::ptr::null(), 0);
                    WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE);
                },
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn send(_: super::Req) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_requests_are_dropped_and_due_ones_fire() {
        let now = Instant::now();
        let (a, ra) = oneshot::channel();
        let (b, rb) = oneshot::channel();
        let (c, rc) = oneshot::channel::<()>();
        drop(rc); // cancelled: its due time must not count
        let mut q = vec![(now - Duration::from_millis(1), a), (now + Duration::from_secs(60), b), (now + Duration::from_millis(1), c)];
        let next = service(&mut q);
        assert_eq!(q.len(), 1, "the due one fired, the cancelled one dropped");
        assert_eq!(next, Some(now + Duration::from_secs(60)));
        assert!(futures::executor::block_on(ra).is_ok());
        drop(rb);
    }
}
