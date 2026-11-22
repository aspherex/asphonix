//! Heap accounting for memory work (`--features memprof`, never in a normal build): a counting global allocator
//! (live bytes per power-of-two size class), a table of the live allocations >= 256 KiB with the backtrace that made
//! them, and a 1 Hz logger that writes `mem live=... classes=...` into $SONIX_LOG and dumps the big-allocation table
//! (with backtraces) to "$SONIX_LOG.bt" every 10 s. Build with `--profile relsym` to get symbols in the dump.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

pub struct Counting;

const CLASSES: usize = 40;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static CLASS_LIVE: [AtomicUsize; CLASSES] = [const { AtomicUsize::new(0) }; CLASSES];
static CLASS_N: [AtomicUsize; CLASSES] = [const { AtomicUsize::new(0) }; CLASSES];
const BIG: usize = 256 * 1024;
const SLOTS: usize = 512;
static LOCK: AtomicBool = AtomicBool::new(false);
struct Slot {
    ptr: usize,
    size: usize,
    bt: Option<Box<std::backtrace::Backtrace>>,
}
static mut TABLE: [Slot; SLOTS] = [const { Slot { ptr: 0, size: 0, bt: None } }; SLOTS];

thread_local! { static IN: Cell<bool> = const { Cell::new(false) }; }

fn class(size: usize) -> usize {
    (usize::BITS - size.max(1).leading_zeros()) as usize
}
fn lock() {
    while LOCK.compare_exchange_weak(false, true, std::sync::atomic::Ordering::Acquire, Relaxed).is_err() {
        std::hint::spin_loop();
    }
}
fn unlock() {
    LOCK.store(false, std::sync::atomic::Ordering::Release);
}

fn add(ptr: *mut u8, size: usize) {
    let live = LIVE.fetch_add(size, Relaxed) + size;
    PEAK.fetch_max(live, Relaxed);
    let c = class(size).min(CLASSES - 1);
    CLASS_LIVE[c].fetch_add(size, Relaxed);
    CLASS_N[c].fetch_add(1, Relaxed);
    if size >= BIG && !IN.with(|i| i.replace(true)) {
        let bt = Box::new(std::backtrace::Backtrace::force_capture());
        lock();
        unsafe {
            let t = &mut *std::ptr::addr_of_mut!(TABLE);
            if let Some(s) = t.iter_mut().find(|s| s.ptr == 0) {
                s.ptr = ptr as usize;
                s.size = size;
                s.bt = Some(bt);
            } else {
                std::mem::forget(bt);
            }
        }
        unlock();
        IN.with(|i| i.set(false));
    }
}
fn sub(ptr: *mut u8, size: usize) {
    LIVE.fetch_sub(size, Relaxed);
    let c = class(size).min(CLASSES - 1);
    CLASS_LIVE[c].fetch_sub(size, Relaxed);
    CLASS_N[c].fetch_sub(1, Relaxed);
    if size >= BIG {
        let mut old = None;
        lock();
        unsafe {
            let t = &mut *std::ptr::addr_of_mut!(TABLE);
            if let Some(s) = t.iter_mut().find(|s| s.ptr == ptr as usize) {
                s.ptr = 0;
                old = s.bt.take();
            }
        }
        unlock();
        // free the Box outside the lock (it re-enters the allocator)
        drop(old);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(p, l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            add(p, l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        sub(p, l.size());
        unsafe { System.dealloc(p, l) };
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            sub(p, l.size());
            add(q, new);
        }
        q
    }
}

pub fn start() {
    let Ok(log) = std::env::var("SONIX_LOG") else { return };
    std::thread::Builder::new()
        .name("memprof".into())
        .spawn(move || {
            use std::io::Write;
            let mut n = 0u32;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                n += 1;
                let mut cl = String::new();
                for c in 0..CLASSES {
                    let b = CLASS_LIVE[c].load(Relaxed);
                    if b >= 64 * 1024 {
                        cl += &format!(" <2^{c}:{:.1}MB/{}", b as f64 / 1048576.0, CLASS_N[c].load(Relaxed));
                    }
                }
                crate::log(&format!("mem live={:.1}MB peak={:.1}MB{cl}", LIVE.load(Relaxed) as f64 / 1048576.0, PEAK.load(Relaxed) as f64 / 1048576.0));
                if n % 10 == 5 {
                    let mut out = format!("== dump t={n}s live={:.1}MB\n", LIVE.load(Relaxed) as f64 / 1048576.0);
                    let mut rows: Vec<(usize, String)> = Vec::new();
                    lock();
                    let snap: Vec<(usize, usize)> = unsafe { (*std::ptr::addr_of!(TABLE)).iter().filter(|s| s.ptr != 0).map(|s| (s.ptr, s.size)).collect() };
                    unlock();
                    // resolve outside the lock: take a short lock per slot to clone-format
                    for (ptr, size) in snap {
                        lock();
                        let s = unsafe { (*std::ptr::addr_of!(TABLE)).iter().find(|s| s.ptr == ptr).and_then(|s| s.bt.as_ref().map(|b| format!("{b}"))) };
                        unlock();
                        if let Some(s) = s {
                            rows.push((size, s));
                        }
                    }
                    rows.sort_by(|a, b| b.0.cmp(&a.0));
                    for (size, bt) in rows {
                        let frames: Vec<&str> = bt.lines().map(|l| l.trim()).filter(|l| l.contains(": ") && !l.starts_with("at ") && !l.contains("memprof") && !l.contains("alloc::") && !l.contains("core::") && !l.contains("std::") && !l.contains("__rust")).take(7).collect();
                        out += &format!("{:.2}MB {}\n", size as f64 / 1048576.0, frames.join(" | "));
                    }
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{log}.bt")) {
                        let _ = f.write_all(out.as_bytes());
                    }
                }
            }
        })
        .ok();
}
