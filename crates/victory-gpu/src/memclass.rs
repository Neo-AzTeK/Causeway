//! What the title said about its memory when it allocated it (diagnostics,
//! `VICTORY_PROBE_CLASSES=1`): the console bits our allocation drops
//! (GPU access, write-combining, memory type), kept per address range, and
//! each frame's tracked and CPU-written pages tallied by them. Tests whether
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
    let mut t = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    let t = t.get_or_insert_with(Tally::default);
    {
        let ranges = RANGES.lock().unwrap_or_else(|e| e.into_inner());
        for at in tracked {
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
