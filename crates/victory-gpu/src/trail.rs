//! Diagnostics: what happened to each title page a disk load left without
//! bytes (`crate::lazy`, `crate::watch`), for the crash report. A title
//! that reads a bad pointer from such a page shows whether the page holds
//! its load's bytes there (the load itself, or where it was read from, is
//! wrong), or other bytes (something wrote it after, or it was filled from
//! another load), and every step that page went through.

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

/// For the crash report: the trail of the page holding `at`, and, for the
/// newest step naming a load, the eight quadwords at `at` in memory and in
/// that load. `None` if the page has no trail (no disk load ever left it
/// without bytes), or another thread holds the trail (a crash on several
/// threads at once).
pub fn note(at: u64) -> Option<String> {
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
}
