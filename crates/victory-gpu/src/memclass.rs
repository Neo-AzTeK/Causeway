//! What the title said about its memory when it allocated it (diagnostics,
//! `VICTORY_PROBE_CLASSES=1`): the console bits our allocation drops
//! (GPU access, write-combining, memory type), kept per address range, and
//! each frame's tracked and written pages tallied by them; and per window of
//! frames, the pages tracked by who wrote them (H0.1). Tests whether
//! the title marks the memory only its GPU uses (`docs/frame-cost.md`, H0.2).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

/// Where the bits came from, then the bits: the protection of a virtual
/// allocation above the host's own, the flags of physical pages, or the
/// memory type of an `XMemAlloc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    Virtual(u32),
    Physical(u64),
    Alloc(u64),
}

/// Start → (end, class), ranges kept apart.
static RANGES: Mutex<BTreeMap<usize, (usize, Class)>> = Mutex::new(BTreeMap::new());

/// Whether the probe is on (read once).
pub fn wanted() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_PROBE_CLASSES").is_ok_and(|v| v == "1"))
}

/// `[at, at + len)` now has `class`; what overlapped it before is cut.
pub fn note(at: usize, len: usize, class: Class) {
    if !wanted() || len == 0 {
        return;
    }
    put(&mut RANGES.lock().unwrap_or_else(|e| e.into_inner()), at, at.saturating_add(len), class);
}

fn put(ranges: &mut BTreeMap<usize, (usize, Class)>, at: usize, end: usize, class: Class) {
    let overlapping: Vec<(usize, (usize, Class))> = ranges
        .range(..end)
        .rev()
        .take_while(|(_, (e, _))| *e > at)
        .map(|(&s, &v)| (s, v))
        .collect();
    for (s, (e, c)) in overlapping {
        ranges.remove(&s);
        if s < at {
            ranges.insert(s, (at, c));
        }
        if e > end {
            ranges.insert(end, (e, c));
        }
    }
    ranges.insert(at, (end, class));
}

fn class_of(ranges: &BTreeMap<usize, (usize, Class)>, at: usize) -> Option<Class> {
    ranges.range(..=at).next_back().filter(|(_, (e, _))| *e > at).map(|(_, (_, c))| *c)
}

#[derive(Default)]
struct Tally {
    frames: u64,
    /// Class → (tracked page-frames, written page-frames).
    pages: HashMap<Option<Class>, (u64, u64)>,
}

static TALLY: Mutex<Option<Tally>> = Mutex::new(None);

/// Frames summed into one log line.
const FRAMES_PER_LINE: u64 = 60;

/// One frame: the pages (addresses) tracked, and those written by the CPU.
pub(crate) fn frame(tracked: impl Iterator<Item = usize>, written: impl Iterator<Item = usize>) {
    if !wanted() {
        return;
    }
    let tracked: Vec<usize> = tracked.collect();
    let mut t = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    let t = t.get_or_insert_with(Tally::default);
    {
        let ranges = RANGES.lock().unwrap_or_else(|e| e.into_inner());
        for &at in &tracked {
            t.pages.entry(class_of(&ranges, at)).or_default().0 += 1;
        }
        for at in written {
            t.pages.entry(class_of(&ranges, at)).or_default().1 += 1;
        }
    }
    t.frames += 1;
    if t.frames == FRAMES_PER_LINE {
        let mut rows: Vec<_> = t.pages.iter().collect();
        rows.sort_by_key(|a| std::cmp::Reverse(a.1.0));
        let text: Vec<String> = rows
            .iter()
            .map(|(c, (tracked, written))| {
                format!("{c:x?}: tracked {} written {}", tracked / t.frames, written / t.frames)
            })
            .collect();
        tracing::info!(
            target: "victory_gpu::trace",
            "classes, 64 KiB pages per frame over {} frames: {}",
            t.frames,
            text.join("; ")
        );
        *t = Tally::default();
    }
    writers_frame(tracked);
}

/// Who wrote a page of title memory: the title's code (a guard fault), the
/// kernel for it (file reads, DirectStorage), or GPU work (write-backs of
/// host copies, list copies into title memory).
#[derive(Debug, Clone, Copy)]
pub enum Writer {
    Cpu = 1,
    Kernel = 2,
    Gpu = 4,
}

/// Over a window of frames: page index → (its class when first seen,
/// writers seen), for every page tracked or written in the window.
#[derive(Default)]
struct Writers {
    frames: u64,
    pages: HashMap<usize, (Option<Class>, u8)>,
}

static WRITERS: Mutex<Option<Writers>> = Mutex::new(None);

/// Frames per writers line: long enough that a page written now and then
/// counts as written.
const FRAMES_PER_WINDOW: u64 = 100;

fn page_entry(w: &mut Writers, page: usize) -> &mut (Option<Class>, u8) {
    w.pages.entry(page).or_insert_with(|| {
        (class_of(&RANGES.lock().unwrap_or_else(|e| e.into_inner()), page * crate::watch::PAGE), 0)
    })
}

/// `[at, at + len)` was written by `who` (diagnostics).
pub(crate) fn wrote(who: Writer, at: usize, len: usize) {
    if !wanted() || len == 0 {
        return;
    }
    let mut w = WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    let w = w.get_or_insert_with(Writers::default);
    for page in at / crate::watch::PAGE..at.saturating_add(len).div_ceil(crate::watch::PAGE) {
        page_entry(w, page).1 |= who as u8;
    }
}

/// Counts the pages tracked in the window by class and by who wrote them
/// (bit 1 the title's code, 2 the kernel, 4 GPU work; 0 nobody), and logs
/// them each window, in MB (`docs/frame-cost.md`, H0.1).
fn writers_frame(tracked: Vec<usize>) {
    let mut w = WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    let w = w.get_or_insert_with(Writers::default);
    for at in tracked {
        page_entry(w, at / crate::watch::PAGE);
    }
    w.frames += 1;
    if w.frames < FRAMES_PER_WINDOW {
        return;
    }
    let mut by: std::collections::BTreeMap<(Option<Class>, u8), u64> = std::collections::BTreeMap::new();
    for &(class, mask) in w.pages.values() {
        *by.entry((class, mask)).or_default() += 1;
    }
    let mb = |pages: u64| (pages * crate::watch::PAGE as u64) >> 20;
    let text: Vec<String> =
        by.iter().map(|((class, mask), pages)| format!("{class:x?} w{mask}: {} MB", mb(*pages))).collect();
    let all: u64 = by.values().sum();
    tracing::info!(
        target: "victory_gpu::trace",
        "writers over {} frames, {} MB of title memory tracked or written: {}",
        w.frames,
        mb(all),
        text.join("; ")
    );
    *w = Writers::default();
    loads_window();
}

/// Loads into title memory over a writers window, per source.
#[derive(Default)]
struct Loads {
    calls: u64,
    /// Bytes the title asked for, and those it got (as far as known).
    asked: u64,
    got: u64,
    /// Calls whose destination range was loaded before in the window.
    same_destination: u64,
    /// Calls whose data (source key) was loaded before in the window.
    same_data: u64,
    same_data_bytes: u64,
    destinations: std::collections::HashSet<(usize, usize)>,
    data: std::collections::HashSet<u64>,
    pages: std::collections::HashSet<usize>,
}

static LOADS: Mutex<BTreeMap<&'static str, Loads>> = Mutex::new(BTreeMap::new());

/// A load of `asked` bytes into title memory at `at` (`got` of them
/// produced) from `source`; `data` names what was read (a file and an
/// offset, say), when known (diagnostics; H0.4 in `docs/frame-cost.md`).
pub fn loaded(source: &'static str, at: usize, asked: usize, got: usize, data: Option<u64>) {
    if !wanted() {
        return;
    }
    let mut loads = LOADS.lock().unwrap_or_else(|e| e.into_inner());
    let l = loads.entry(source).or_default();
    l.calls += 1;
    l.asked += asked as u64;
    l.got += got as u64;
    if !l.destinations.insert((at, asked)) {
        l.same_destination += 1;
    }
    if let Some(d) = data
        && !l.data.insert(d)
    {
        l.same_data += 1;
        l.same_data_bytes += got as u64;
    }
    let page = crate::watch::PAGE;
    l.pages.extend(at / page..at.saturating_add(asked).div_ceil(page));
}

fn loads_window() {
    let mut loads = LOADS.lock().unwrap_or_else(|e| e.into_inner());
    let mb = |b: u64| b >> 20;
    let text: Vec<String> = loads
        .iter()
        .map(|(s, l)| {
            format!(
                "{s}: {} calls, asked {} MB, got {} MB, {} MB of pages, same destination {}, same data {} ({} MB)",
                l.calls,
                mb(l.asked),
                mb(l.got),
                mb(l.pages.len() as u64 * crate::watch::PAGE as u64),
                l.same_destination,
                l.same_data,
                mb(l.same_data_bytes)
            )
        })
        .collect();
    tracing::info!(target: "victory_gpu::trace", "loads over the window: {}", text.join("; "));
    loads.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_range_cuts_the_old() {
        let mut r = BTreeMap::new();
        r.insert(0x1000, (0x5000, Class::Virtual(1)));
        put(&mut r, 0x2000, 0x3000, Class::Alloc(2));
        assert_eq!(class_of(&r, 0x1800), Some(Class::Virtual(1)));
        assert_eq!(class_of(&r, 0x2800), Some(Class::Alloc(2)));
        assert_eq!(class_of(&r, 0x4000), Some(Class::Virtual(1)));
        assert_eq!(class_of(&r, 0x5000), None);
    }
}
