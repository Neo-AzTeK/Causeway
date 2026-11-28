//! Diagnostics: what happened to each title page a disk load left without
//! bytes (`crate::lazy`, `crate::watch`), for the crash report. A title
//! that reads a bad pointer from such a page shows whether the page holds
//! its load's bytes there (the load itself, or where it was read from, is
//! wrong), or other bytes (something wrote it after, or it was filled from
//! another load), and every step that page went through. For every page,
//! also the last writes the title's code did not make itself (write-backs
//! of host copies, kernel reads) and remaps: who last wrote memory a bad
//! pointer came from.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::lazy::Load;
use crate::watch::PAGE;

/// Steps kept per page, newest last.
const STEPS: usize = 8;

struct Step {
    /// Seconds since the first step of the process.
    at: f32,
    what: &'static str,
    /// The load and where in it the page starts, where the step names one.
    load: Option<(Arc<Load>, usize)>,
}

static TRAIL: Mutex<Option<HashMap<usize, Vec<Step>>>> = Mutex::new(None);

fn seconds() -> f32 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32()
}

/// Page `page` (a page number) went through `what`, with `load` at that
/// offset where given.
pub(crate) fn step(page: usize, what: &'static str, load: Option<(&Arc<Load>, usize)>) {
    let at = seconds();
    let mut trail = TRAIL.lock().unwrap_or_else(|e| e.into_inner());
    let steps = trail
        .get_or_insert_with(HashMap::new)
        .entry(page)
        .or_default();
    push(
        steps,
        Step {
            at,
            what,
            load: load.map(|(l, o)| (l.clone(), o)),
        },
    );
}

/// Adds `step`; when full, the oldest step after the first few goes (how
/// the page was first left and filled stays).
fn push(steps: &mut Vec<Step>, step: Step) {
    const KEPT: usize = 3;
    if steps.len() == STEPS {
        steps.remove(KEPT);
    }
    steps.push(step);
}

/// [`step`] for pages that have a trail already (every page disk loads
/// once left without bytes): the steps of other writers there.
pub(crate) fn step_if_seen(first: usize, last: usize, what: &'static str) {
    let mut trail = TRAIL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(trail) = trail.as_mut() else {
        return;
    };
    let at = seconds();
    // A range may be far larger than the trail (a title's saturated length).
    let pages: Vec<usize> = if last.saturating_sub(first) > trail.len() {
        trail
            .keys()
            .copied()
            .filter(|p| (first..last).contains(p))
            .collect()
    } else {
        (first..last).filter(|p| trail.contains_key(p)).collect()
    };
    for page in pages {
        if let Some(steps) = trail.get_mut(&page) {
            push(
                steps,
                Step {
                    at,
                    what,
                    load: None,
                },
            );
        }
    }
}

/// Writes kept per page, newest last.
const WRITES: usize = 4;

/// Pages one write is kept for at most: a title's lengths may be saturated
/// (`fread`'s size times count).
const WRITE_PAGES: usize = 1024;

/// A write into title memory that the title's own code did not make, or a
/// change of what an address shows.
struct Write {
    at: f32,
    what: &'static str,
    /// Where in the page, and how many bytes there.
    from: u32,
    len: u32,
    /// The host copy whose bytes went there: its title address and length.
    copy: Option<(u64, u64)>,
}

/// Every page's last writes, whether or not a disk load left it: a title
/// that reads a bad pointer shows who last wrote the memory it came from.
static WRITTEN: Mutex<Option<HashMap<usize, Vec<Write>>>> = Mutex::new(None);

/// `what` wrote `[at, at + len)` of title memory (from host copy `copy`
/// where given), or changed what it shows.
pub(crate) fn wrote(at: usize, len: usize, what: &'static str, copy: Option<(u64, u64)>) {
    let now = seconds();
    let end = at.saturating_add(len);
    let first = at / PAGE;
    let last = end.div_ceil(PAGE).min(first + WRITE_PAGES);
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    let written = written.get_or_insert_with(HashMap::new);
    for page in first..last {
        let from = at.max(page * PAGE);
        let to = end.min((page + 1).saturating_mul(PAGE));
        let writes = written.entry(page).or_default();
        if writes.len() == WRITES {
            writes.remove(0);
        }
        writes.push(Write {
            at: now,
            what,
            from: (from - page * PAGE) as u32,
            len: (to - from) as u32,
            copy,
        });
    }
}

/// Whether quadword `q` holds only infinities: four halves or two floats,
/// each `+inf` or `-inf` (an empty box's corners).
fn infinities(q: u64) -> bool {
    let halves = (0..4).all(|k| matches!((q >> (16 * k)) as u16, 0x7c00 | 0xfc00));
    let floats = (0..2).all(|k| matches!((q >> (32 * k)) as u32, 0x7f80_0000 | 0xff80_0000));
    halves || floats
}

/// Whether ranges GPU work may have written in place are scanned too
/// (`VICTORY_PROBE_EMPTY_BOUNDS=1`): each declared range is read before
/// and after every submission; bounds found only after appeared while the
/// lists ran (written by the GPU, or by a title thread meanwhile).
pub(crate) fn scan_in_place() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_PROBE_EMPTY_BOUNDS").is_ok_and(|v| v == "1"))
}

/// After a write that `what` names into `[at, at + len)` (pages [`wrote`]
/// keeps at most): the first quadword of -1 beside one of infinities in
/// each page, the empty bounds the title faulted on reading as a pointer,
/// becomes a write of the page's trail, so that a crash report tells
/// whether that write brought them.
pub(crate) fn scan_empty_bounds(at: usize, len: usize, what: &'static str) {
    scan_new_empty_bounds(at, len, &[], what);
}

/// [`scan_empty_bounds`], leaving out the bounds at `before` (addresses
/// [`empty_bounds`] gave before the write): what is left appeared since.
pub(crate) fn scan_new_empty_bounds(at: usize, len: usize, before: &[usize], what: &'static str) {
    let mut last_page = None;
    for hit in empty_bounds(at, len) {
        if last_page == Some(hit / PAGE) || before.binary_search(&hit).is_ok() {
            continue;
        }
        last_page = Some(hit / PAGE);
        wrote(hit, 16, what, None);
        // Also where else the title shows the page: the address its code
        // reads may be another mapping of it.
        for other in crate::watch::shown_elsewhere(hit) {
            wrote(other + hit % PAGE, 16, what, None);
        }
    }
}

/// The addresses in `[at, at + len)` (pages [`wrote`] keeps at most), in
/// order, of each quadword of -1 beside one of infinities.
pub(crate) fn empty_bounds(at: usize, len: usize) -> Vec<usize> {
    let end = at.saturating_add(len).min((at / PAGE * PAGE).saturating_add(WRITE_PAGES * PAGE));
    let mut hits = Vec::new();
    let mut page = at / PAGE * PAGE;
    while page < end {
        let (from, to) = (at.max(page) & !7, end.min(page.saturating_add(PAGE)));
        let n = to.saturating_sub(from) / 8;
        let mut q = vec![0u64; n];
        if n >= 2 && crate::host::read_kernel(from as *const u8, q.as_mut_ptr().cast(), 8 * n) {
            hits.extend((0..n).filter(|&i| {
                q[i] == u64::MAX && (q.get(i + 1).is_some_and(|&b| infinities(b)) || (i > 0 && infinities(q[i - 1])))
            }).map(|i| from + 8 * i));
        }
        let Some(next) = page.checked_add(PAGE) else { break };
        page = next;
    }
    hits
}

/// The last writes of the page holding `at` ([`wrote`]), oldest first.
fn writes(at: usize) -> Option<String> {
    let written = WRITTEN.try_lock().ok()?;
    let writes = written.as_ref()?.get(&(at / PAGE))?;
    let now = seconds();
    let text: Vec<String> = writes
        .iter()
        .map(|w| {
            let copy = w.copy.map_or(String::new(), |(a, l)| format!(" from the copy of {a:#x}+{l:#x}"));
            format!(
                "{:.1}s ago {} +{:#x}..+{:#x}{copy}",
                now - w.at,
                w.what,
                w.from,
                w.from + w.len
            )
        })
        .collect();
    Some(format!("page {:#x} last written: {}", at / PAGE * PAGE, text.join("; ")))
}

/// For the crash report: [`loaded`] and [`writes`] of the page holding
/// `at`, and the GPU copies holding it (`crate::shadow::holding`). `None`
/// if it has none, or another thread holds them (a crash on several threads
/// at once).
pub fn note(at: u64) -> Option<String> {
    let parts: Vec<String> =
        [loaded(at), writes(at as usize), crate::shadow::holding(at), crate::inplace::physical(at)].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join("\n    "))
}

/// The trail of the page holding `at`, and, for the newest step naming a
/// load, the eight quadwords at `at` in memory and in that load. `None` if
/// the page has no trail (no disk load ever left it without bytes).
fn loaded(at: u64) -> Option<String> {
    let at = at as usize;
    let page = at / PAGE;
    let (steps, load) = {
        let trail = TRAIL.try_lock().ok()?;
        let steps = trail.as_ref()?.get(&page)?;
        let now = seconds();
        let text: Vec<String> = steps
            .iter()
            .map(|s| match &s.load {
                Some((l, o)) => format!(
                    "{:.1}s ago {} (load {} +{o:#x})",
                    now - s.at,
                    s.what,
                    l.name()
                ),
                None => format!("{:.1}s ago {}", now - s.at, s.what),
            })
            .collect();
        let load = steps.iter().rev().find_map(|s| s.load.clone());
        (text.join("; "), load)
    };
    let mut out = format!("page {:#x}: {steps}", page * PAGE);
    if let Some((load, offset)) = load {
        let within = at % PAGE;
        let n = 64.min(PAGE - within);
        let mut memory = vec![0u8; n];
        let mut from_load = vec![0u8; n];
        let read = crate::host::read_kernel(at as *const u8, memory.as_mut_ptr(), n);
        let loaded = load.bytes(offset + within, &mut from_load);
        let words = |b: &[u8]| {
            b.as_chunks::<8>()
                .0
                .iter()
                .map(|w| format!("{:#x}", u64::from_le_bytes(*w)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let differ = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
        out.push_str(&match (read, loaded) {
            (true, true) if memory == from_load => {
                format!("\n    memory = load: {}", words(&memory))
            }
            (true, true) => format!(
                "\n    memory: {}\n    load:   {} ({} of {n} bytes differ)",
                words(&memory),
                words(&from_load),
                differ(&memory, &from_load)
            ),
            (false, true) => format!("\n    memory not readable; load: {}", words(&from_load)),
            (_, false) => "\n    the load's bytes could not be read".to_string(),
        });
        // The whole page against the load: where the title wrote after the
        // fill, and how much.
        let mut page_bytes = vec![0u8; PAGE];
        let mut load_page = vec![0u8; PAGE];
        if crate::host::read_kernel((page * PAGE) as *const u8, page_bytes.as_mut_ptr(), PAGE)
            && load.bytes(offset, &mut load_page)
        {
            let first = page_bytes.iter().zip(&load_page).position(|(a, b)| a != b);
            out.push_str(&format!(
                "\n    whole page: {} of {PAGE} bytes differ from the load{}",
                differ(&page_bytes, &load_page),
                first.map_or(String::new(), |f| format!(", first at +{f:#x}"))
            ));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>);

    impl crate::lazy::Source for Bytes {
        fn size(&self) -> usize {
            self.0.len()
        }
        fn whole(&self) -> bool {
            false
        }
        fn read(&self, at: usize, out: &mut [u8]) -> bool {
            out.copy_from_slice(&self.0[at..at + out.len()]);
            true
        }
    }

    #[test]
    fn a_note_tells_the_steps_and_where_memory_and_load_differ() {
        let mut memory = vec![0u8; 3 * PAGE];
        let base = memory.as_ptr() as usize;
        let page = base.next_multiple_of(PAGE);
        let loaded: Vec<u8> = (0..PAGE).map(|i| (i * 7) as u8).collect();
        let at = page - base;
        memory[at..at + PAGE].copy_from_slice(&loaded);
        memory[at + 0x48] ^= 0xff;
        let load = Load::new(Box::new(Bytes(loaded)));
        step(page / PAGE, "left without bytes", Some((&load, 0)));
        step_if_seen(page / PAGE, usize::MAX, "written by the title");
        let n = note((page + 0x40) as u64).expect("a trail");
        assert!(n.contains("left without bytes (load"), "{n}");
        assert!(n.contains("written by the title"), "{n}");
        assert!(n.contains("1 of 64 bytes differ"), "{n}");
        assert!(
            n.contains("1 of 65536 bytes differ from the load, first at +0x48"),
            "{n}"
        );
        assert!(note((page + PAGE) as u64).is_none());
        drop(memory);
    }

    #[test]
    fn a_write_of_empty_bounds_is_noted() {
        // Only read: plain host memory will do.
        let mut mem = vec![0u64; 3 * PAGE / 8];
        let base = mem.as_ptr() as usize;
        let page = base.next_multiple_of(PAGE);
        let at = |offset: usize| (page + offset - base) / 8;
        mem[at(0x50)] = u64::MAX;
        mem[at(0x58)] = 0x7f80_0000_ff80_0000;
        mem[at(PAGE + 0x100)] = 0xfc00_fc00_7c00_7c00;
        mem[at(PAGE + 0x108)] = u64::MAX;
        mem[at(PAGE + 0x200)] = u64::MAX;
        scan_empty_bounds(page + 8, 2 * PAGE - 64, "test: empty bounds");
        let n = note((page + 0x50) as u64).expect("a note");
        assert!(n.contains("test: empty bounds +0x50..+0x60"), "floats after -1: {n}");
        let n = note((page + PAGE) as u64).expect("a note");
        assert!(n.contains("test: empty bounds +0x108..+0x118"), "halves before -1: {n}");
        assert!(!n.contains("+0x200"), "-1 alone is not one: {n}");
        drop(mem);
    }

    #[test]
    fn only_empty_bounds_that_appeared_are_noted() {
        // Only read: plain host memory will do.
        let mut mem = vec![0u64; 2 * PAGE / 8];
        let base = mem.as_ptr() as usize;
        let page = base.next_multiple_of(PAGE);
        let at = |offset: usize| (page + offset - base) / 8;
        mem[at(0x40)] = u64::MAX;
        mem[at(0x48)] = 0x7f80_0000_7f80_0000;
        let before = empty_bounds(page, PAGE);
        assert_eq!(before, [page + 0x40]);
        mem[at(0x300)] = u64::MAX;
        mem[at(0x308)] = 0xff80_0000_ff80_0000;
        scan_new_empty_bounds(page, PAGE, &before, "test: appeared");
        let n = note((page + 0x40) as u64).expect("a note");
        assert!(n.contains("test: appeared +0x300..+0x310"), "the new one: {n}");
        assert!(!n.contains("+0x40..+0x50"), "not the one there before: {n}");
        drop(mem);
    }

    #[test]
    fn a_note_tells_the_last_writes_of_any_page() {
        // An address no test maps: the trail only keeps numbers.
        let page = 0x7_1234_0000usize;
        for k in 0..WRITES + 1 {
            wrote(page + 0x100 * k, 0x10, "written by GPU work", Some((0x7_1230_0000, 0x8_0000)));
        }
        wrote(page + PAGE - 8, 16, "written by the runtime or the kernel", None);
        let n = note((page + 0x40) as u64).expect("writes");
        assert!(!n.contains("+0x0..+0x10 "), "the oldest write went: {n}");
        assert!(n.contains("written by GPU work +0x400..+0x410 from the copy of 0x712300000+0x80000"), "{n}");
        assert!(n.contains("written by the runtime or the kernel +0xfff8..+0x10000"), "{n}");
        let next = note((page + PAGE) as u64).expect("the write's second page");
        assert!(next.contains("+0x0..+0x8"), "{next}");
        assert_eq!(n.matches("ago").count(), WRITES);
    }
}
