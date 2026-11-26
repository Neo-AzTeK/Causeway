//! A test mode for getting through slow parts of a title, and a per-frame
//! profile (diagnostics builds).
//!
//! `VICTORY_DRAW_BUDGET_MS=<n>`: each frame may record only as many draws
//! as should fit in `n` ms, judged from the frame before; the rest are
//! skipped. Dispatches, copies, clears and synchronization all run, so a
//! frame can still take longer. Small draws (fullscreen passes, quads)
//! always run, so a finished picture still reaches the screen. H in the
//! game window draws the next frames in full, then keeps the last of them in
//! the window until H is pressed again. The title is not stopped: it goes on
//! behind that picture, under the limit.
//!
//! `VICTORY_PROFILE=<file>`: one CSV line for each presented frame.
//!
//! shortcut: the limit counts draws in the order they are recorded and
//! takes them to cost alike; what a skipped draw would have written once
//! stays unwritten after full drawing is back.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Draws of up to this many vertices or indices are never skipped.
const SMALL: u32 = 6;
/// The fewest frames drawn in full before the picture is held: effects that
/// build over several frames need them.
const SETTLE: u64 = 8;
/// The most: past [`SETTLE`] the picture waits for the pipelines still being
/// made, whose draws are dropped until then, but a title may always have one.
const SETTLE_LIMIT: u64 = 64;
/// The fewest other draws a limited frame may record.
const FLOOR: u64 = 32;

static BUDGET_MS: LazyLock<Option<f64>> = LazyLock::new(|| {
    std::env::var("VICTORY_DRAW_BUDGET_MS").ok()?.parse().ok().filter(|ms| *ms > 0.0)
});
static FULL: AtomicBool = AtomicBool::new(false);
/// Full frames drawn since H asked for them.
static SETTLED: AtomicU64 = AtomicU64::new(0);
/// The window keeps the picture it has.
static HELD: AtomicBool = AtomicBool::new(false);
/// The frame had full drawing at some point.
static WAS_FULL: AtomicBool = AtomicBool::new(false);
static ALLOWED: AtomicU64 = AtomicU64::new(u64::MAX);
/// This frame's limited draws: recorded, skipped; and the small ones.
static COUNTS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Whether to skip a draw of `size` vertices or indices (none: not known).
pub(crate) fn skip_draw(size: Option<u32>) -> bool {
    if size.is_some_and(|n| n <= SMALL) {
        COUNTS[2].fetch_add(1, Relaxed);
        return false;
    }
    let skip = BUDGET_MS.is_some()
        && !FULL.load(Relaxed)
        && COUNTS[0].load(Relaxed) >= ALLOWED.load(Relaxed);
    COUNTS[usize::from(skip)].fetch_add(1, Relaxed);
    skip
}

/// Polls H on a thread of its own, as the keyboard pad polls its keys: the
/// window shown need not be one whose messages the host sees, and a slow
/// frame must not delay the switch.
fn watch_h() {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    std::thread::spawn(|| {
        let mut was = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(15));
            // SAFETY: a plain query.
            let down = unsafe { GetAsyncKeyState(i32::from(b'H')) } < 0;
            if down && !was {
                let mut process = 0;
                // SAFETY: plain queries; a null window gives process 0.
                unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut process)) };
                if process == std::process::id() {
                    toggle_full();
                }
            }
            was = down;
        }
    });
}

/// H was pressed while one of our windows had the focus.
fn toggle_full() {
    if BUDGET_MS.is_none() {
        return;
    }
    if HELD.swap(false, Relaxed) {
        tracing::info!("draw budget: H pressed, picture released");
        return;
    }
    let full = !FULL.fetch_xor(true, Relaxed);
    if full {
        WAS_FULL.store(true, Relaxed);
    }
    SETTLED.store(0, Relaxed);
    HELD.store(false, Relaxed);
    tracing::info!(full, "draw budget: H pressed");
}

/// Whether the window is to keep its picture: frames are not presented.
pub(crate) fn picture_held() -> bool {
    HELD.load(Relaxed)
}

/// After a presented full frame: holds the picture once enough were drawn.
/// A held picture ends the full drawing, so the title goes on quickly.
fn settle() {
    if !FULL.load(Relaxed) || HELD.load(Relaxed) {
        return;
    }
    let drawn = SETTLED.fetch_add(1, Relaxed) + 1;
    let waiting = crate::pipeline::making();
    if drawn >= SETTLE_LIMIT || (drawn >= SETTLE && waiting == 0) {
        HELD.store(true, Relaxed);
        FULL.store(false, Relaxed);
        tracing::info!(drawn, waiting, "draw budget: picture held; H goes on");
    }
}

struct Frames {
    started: Instant,
    number: u64,
    /// The running totals at the last frame: microseconds preparing,
    /// waiting for the GPU, copying, refreshing textures, copying back;
    /// dispatches.
    totals: [u64; 6],
    file: Option<std::fs::File>,
}

static FRAMES: LazyLock<Mutex<Frames>> = LazyLock::new(|| {
    let mut file = std::env::var_os("VICTORY_PROFILE").and_then(|p| std::fs::File::create(p).ok());
    if let Some(f) = &mut file {
        let _ = writeln!(
            f,
            "frame,ms,full,prepare_ms,gpu_ms,copies_ms,textures_ms,copy_back_ms,rest_ms,draws,skipped,small,dispatches"
        );
    }
    if let Some(budget) = *BUDGET_MS {
        tracing::info!(budget, "draw budget on: H switches full drawing");
        watch_h();
    }
    Mutex::new(Frames { started: Instant::now(), number: 0, totals: [0; 6], file })
});

/// At each presented frame. `totals` as [`Frames::totals`]; a total lower
/// than the last time was reset in between.
pub(crate) fn frame(totals: [u64; 6]) {
    let mut frames = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    let ms = frames.started.elapsed().as_secs_f64() * 1000.0;
    frames.started = Instant::now();
    frames.number += 1;
    let spent: [u64; 6] = std::array::from_fn(|i| {
        totals[i].checked_sub(frames.totals[i]).unwrap_or(totals[i])
    });
    frames.totals = totals;
    let [drawn, skipped, small] = [0, 1, 2].map(|i| COUNTS[i].swap(0, Relaxed));
    let full = WAS_FULL.swap(FULL.load(Relaxed), Relaxed) || BUDGET_MS.is_none();
    if let Some(budget) = *BUDGET_MS
        && !full
    {
        let allowed = if ms > budget {
            ((drawn as f64 * budget / ms) as u64).max(FLOOR)
        } else {
            let before = ALLOWED.load(Relaxed);
            let reached = if before == u64::MAX { drawn } else { before.max(FLOOR) };
            reached.saturating_mul(2)
        };
        ALLOWED.store(allowed, Relaxed);
    }
    let number = frames.number;
    if let Some(f) = &mut frames.file {
        let [prepare, gpu, copies, textures, back] = [0, 1, 2, 3, 4].map(|i| spent[i] as f64 / 1000.0);
        let rest = (ms - prepare - gpu - back).max(0.0);
        let _ = writeln!(
            f,
            "{number},{ms:.1},{},{prepare:.1},{gpu:.1},{copies:.1},{textures:.1},{back:.1},{rest:.1},{drawn},{skipped},{small},{}",
            u8::from(full),
            spent[5]
        );
    }
    // A full frame is one that reached the window with no limit all along.
    if full && BUDGET_MS.is_some() {
        settle();
    }
}
