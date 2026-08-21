//! Stats for nerds. `PLAIT_STATS=1` turns the overlay on.
//!
//! Frame timing here measures *how fast we can redraw*, not how often we do.
//! GPUI is reactive and idles at zero frames; the overlay requests an animation
//! frame every render to force a continuous loop. That is the number you want
//! when judging scroll smoothness, and it is not what the app costs at rest.
//!
//! It is also a ceiling, not a throughput. GPUI paces animation frames off
//! `CVDisplayLink` — one callback per display refresh — so the fps reading is
//! the display's *current* rate for as long as we stay inside its budget. That
//! rate is not a constant: Low Power Mode pins a ProMotion panel to 60Hz, as
//! does running on battery under some settings, so the same binary reads 120
//! plugged in and 60 unplugged with nothing about the app having changed.
//!
//! Which is why `best` is on the line. It is the shortest interval in the ring
//! — the display's real beat, the one frame we know we made. If `best` and
//! `p50` agree we never missed one, and the fps figure is the panel's, not
//! ours. Only when `p50` drifts above `best` are we the reason.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Wraps the system allocator to track live heap bytes. Two relaxed atomics per
/// allocation — a few nanoseconds, and worth it to see the heap move while you
/// scroll. Swap back to `System` if you ever benchmark allocation throughput.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(n, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

pub fn enabled() -> bool {
    std::env::var("PLAIT_STATS").is_ok_and(|v| v != "0")
}

const RING: usize = 120;

pub struct Stats {
    ring: [f32; RING],
    n: usize,
    last: Option<Instant>,
    /// Written by whichever view is on screen; it owns the cell, we only read.
    pub rows_drawn: Rc<Cell<usize>>,
    pub total_rows: usize,
    /// One-off load timings, captured before the window opened.
    pub load: String,
}

impl Stats {
    pub fn new(rows_drawn: Rc<Cell<usize>>, total_rows: usize, load: String) -> Self {
        Self { ring: [0.0; RING], n: 0, last: None, rows_drawn, total_rows, load }
    }

    /// After the view rebuilt its rows — a layout or algorithm change. Both
    /// numbers are one-off measurements of a load that has now happened twice,
    /// and an overlay reporting the first one is worse than no overlay.
    pub fn reloaded(&mut self, total_rows: usize, load: String) {
        self.total_rows = total_rows;
        self.load = load;
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(prev) = self.last {
            self.ring[self.n % RING] = (now - prev).as_secs_f32() * 1000.0;
            self.n += 1;
        }
        self.last = Some(now);
    }

    pub fn frames(&self) -> String {
        let count = self.n.min(RING);
        if count < 8 {
            return "measuring…".into();
        }
        let mut v: Vec<f32> = self.ring[..count].to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = v[count / 2].max(0.001);
        let p99 = v[count * 99 / 100];
        let best = v[0];
        format!(
            "{:>3.0} fps    frame p50 {p50:>5.2}ms   p99 {p99:>5.2}ms   best {best:>5.2}ms",
            1000.0 / p50
        )
    }

    /// Live proof that virtualization is working: rows built this frame vs rows
    /// that exist. If the left number tracks the right one, it isn't.
    pub fn rows(&self) -> String {
        format!("rows {:>4} drawn / {:>9} total", self.rows_drawn.get(), self.total_rows)
    }

    pub fn heap(&self) -> String {
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        format!("heap {:>7.1} MB   peak {:>7.1} MB", mb(LIVE.load(Relaxed)), mb(PEAK.load(Relaxed)))
    }
}
