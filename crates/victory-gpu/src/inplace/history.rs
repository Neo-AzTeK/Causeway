//! Diagnostics, for the crash report: the title addresses each section page
//! was mapped at over the run, and the in-place buffers placed over it, so
//! that a crash on memory an address shows tells whether the GPU could
//! have written it through a buffer placed for another address (the same
//! physical page shown twice, or one shown before at another address).

use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use crate::shadow::Shadow;

/// A title view the runtime mapped: `len` bytes at `at` from `offset` of
/// section `section`.
struct Mapped {
    section: usize,
    offset: u64,
    len: u64,
    at: u64,
    when: Instant,
}

/// An in-place buffer placed in a section: title addresses `[start, start
/// + len)` from `offset` of section `section`.
struct Placed {
    section: usize,
    offset: u64,
    len: u64,
    start: u64,
    when: Instant,
    copy: Weak<Shadow>,
}

/// Entries kept of each kind: the oldest half goes when full (streaming
/// maps many views a second; a run places some thousands of buffers).
const MAPPED_KEPT: usize = 1 << 17;
const PLACED_KEPT: usize = 1 << 16;

static MAPPED: Mutex<Vec<Mapped>> = Mutex::new(Vec::new());
static PLACED: Mutex<Vec<Placed>> = Mutex::new(Vec::new());

fn push<T>(list: &Mutex<Vec<T>>, kept: usize, item: T) {
    let mut list = list.lock().unwrap_or_else(|e| e.into_inner());
    if list.len() >= kept {
        list.drain(..kept / 2);
    }
    list.push(item);
}

/// The runtime mapped `[at, at + len)` from `offset` of `section`.
pub(super) fn mapped(at: u64, len: u64, section: usize, offset: u64) {
    push(&MAPPED, MAPPED_KEPT, Mapped { section, offset, len, at, when: Instant::now() });
}

/// `copy` was made in place; noted where it lies in a section.
pub(crate) fn placed(copy: &Arc<Shadow>) {
    if let Some((start, len, section, offset)) = copy.placed_run() {
        push(&PLACED, PLACED_KEPT, Placed { section, offset, len, start, when: Instant::now(), copy: Arc::downgrade(copy) });
    }
}

/// The note for title address `at`: the section page it shows now (`now`:
/// none where it shows none, `None` where the views were busy), the pages
/// it showed before, and for each the addresses it was mapped at and the
/// in-place buffers placed over it, newest first. None where no section
/// page was mapped there (in the views kept), or another thread holds the
/// lists (a crash on several threads at once).
pub(super) fn note(at: u64, now: Option<Option<(usize, u64)>>, page_size: u64) -> Option<String> {
    let mapped = MAPPED.try_lock().ok()?;
    let placed = PLACED.try_lock().ok()?;
    let page = at / page_size * page_size;
    let t = Instant::now();
    let ago = |w: Instant| t.duration_since(w).as_secs_f64();
    let mut pages: Vec<(usize, u64)> = Vec::new();
    if let Some(Some(p)) = now {
        pages.push(p);
    }
    for m in mapped.iter().rev() {
        if m.at <= page && page < m.at + m.len {
            let p = (m.section, m.offset + (page - m.at));
            if !pages.contains(&p) {
                pages.push(p);
            }
            if pages.len() >= 3 {
                break;
            }
        }
    }
    // No line for memory no section page was mapped at (in the views kept).
    if pages.is_empty() {
        return None;
    }
    let mut out = vec![match now {
        Some(Some((s, o))) => format!("shows section {s:#x} page +{o:#x} now"),
        Some(None) => "shows no section page now".to_string(),
        None => "section views busy".to_string(),
    }];
    let listed = |v: Vec<String>| if v.is_empty() { "none".to_string() } else { v.join(", ") };
    for &(section, offset) in &pages {
        let at_addresses: Vec<String> = mapped
            .iter()
            .rev()
            .filter(|m| m.section == section && m.offset <= offset && offset < m.offset + m.len)
            .take(6)
            .map(|m| format!("{:#x} {:.1}s ago", m.at + (offset - m.offset), ago(m.when)))
            .collect();
        let buffers: Vec<String> = placed
            .iter()
            .rev()
            .filter(|p| p.section == section && p.offset <= offset && offset < p.offset + p.len)
            .take(6)
            .map(|p| {
                let state = match p.copy.upgrade() {
                    Some(c) if c.is_let_go() => "let go",
                    Some(_) => "open",
                    None => "released",
                };
                format!(
                    "{:#x}+{:#x} (this page at {:#x}) {:.1}s ago, {state}",
                    p.start,
                    p.len,
                    p.start + (offset - p.offset),
                    ago(p.when)
                )
            })
            .collect();
        out.push(format!(
            "section {section:#x} page +{offset:#x}: mapped at {}; in-place buffers over it: {}",
            listed(at_addresses),
            listed(buffers)
        ));
    }
    Some(format!("physical: {}", out.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page shown at two addresses over time, and an address that never
    /// showed a section page.
    #[test]
    fn a_note_names_the_addresses_a_page_was_mapped_at() {
        const P: u64 = 0x10000;
        // Section ids no other test uses.
        let section = 0x5ec7_0001;
        mapped(0x7_7700_0000, 2 * P, section, 4 * P);
        mapped(0x7_7800_0000, P, section, 5 * P);
        let n = note(0x7_7700_0000 + P + 0x40, Some(None), P).expect("a note");
        assert!(n.contains("shows no section page now"), "{n}");
        assert!(n.contains(&format!("section {section:#x} page +0x50000")), "{n}");
        assert!(n.contains("0x778000000") && n.contains("0x777010000"), "both addresses: {n}");
        assert!(n.contains("in-place buffers over it: none"), "{n}");
        assert!(note(0x7_7900_0000, Some(None), P).is_none(), "never mapped from a section");
    }
}
