//! Probe (diagnostics): how much of the host textures' memory is for mip
//! levels whose title memory is not mapped (problem `placed-texture-slots`).
//!
//! The title places a texture's whole chain of levels in its address space
//! and may map only some of them (streaming). A host texture is committed
//! with every level, so levels the title does not hold still take video
//! memory. Counted twice: when a host texture is made, and when its
//! contents are first uploaded (`texture.rs`), as console bytes of the
//! levels and the host bytes in the same share.

use std::sync::Mutex;

use victory_formats::texture::Layout;

/// Byte spans `[start, end)` of each mip level inside one slice of
/// `layout`. Levels sharing a block (the mip tail) share its span.
pub(crate) fn spans(layout: &Layout) -> Vec<(u64, u64)> {
    let mut starts: Vec<u64> = layout.mips.iter().map(|m| m.offset).collect();
    starts.sort_unstable();
    starts.dedup();
    layout
        .mips
        .iter()
        .map(|m| {
            let end = starts.iter().copied().find(|&s| s > m.offset).unwrap_or(layout.slice_size);
            (m.offset, end.max(m.offset))
        })
        .collect()
}

/// Per level of `layout` placed at `memory`: whether its memory is mapped
/// and readable in every slice.
pub(crate) fn held(memory: *const u8, layout: &Layout) -> Vec<bool> {
    spans(layout)
        .iter()
        .map(|&(start, end)| {
            (0..layout.slices.max(1)).all(|s| {
                let at = s as u64 * layout.slice_size + start;
                crate::texture::committed(memory.wrapping_add(at as usize), (end - start) as usize)
            })
        })
        .collect()
}

/// Totals for one moment: textures, console bytes, console bytes not
/// held, host bytes, host bytes in the share not held.
#[derive(Default)]
struct Totals {
    textures: u64,
    console: u64,
    unheld: u64,
    host: u64,
    host_unheld: u64,
}

static MADE: Mutex<Totals> = Mutex::new(Totals { textures: 0, console: 0, unheld: 0, host: 0, host_unheld: 0 });
static UPLOADED: Mutex<Totals> = Mutex::new(Totals { textures: 0, console: 0, unheld: 0, host: 0, host_unheld: 0 });

/// Counts a host texture of `host_bytes` for the title texture of `layout`
/// at `memory`, at `moment` ("made" or "first upload"). Textures of one
/// level are skipped: they are held whole or not used.
pub(crate) fn count(moment: &'static str, memory: *const u8, layout: &Layout, host_bytes: u64) {
    if layout.mips.len() < 2 || memory.is_null() {
        return;
    }
    let spans = spans(layout);
    let held = held(memory, layout);
    let slices = u64::from(layout.slices.max(1));
    let mut seen = std::collections::BTreeSet::new();
    let (mut console, mut unheld) = (0u64, 0u64);
    for (&(start, end), &h) in spans.iter().zip(&held) {
        if !seen.insert(start) {
            continue;
        }
        let n = (end - start) * slices;
        console += n;
        if !h {
            unheld += n;
        }
    }
    let share = if console == 0 { 0 } else { (u128::from(host_bytes) * u128::from(unheld) / u128::from(console)) as u64 };
    let totals = if moment == "made" { &MADE } else { &UPLOADED };
    let mut t = totals.lock().unwrap_or_else(|e| e.into_inner());
    t.textures += 1;
    t.console += console;
    t.unheld += unheld;
    t.host += host_bytes;
    t.host_unheld += share;
    if t.textures.is_power_of_two() || t.textures % 512 == 0 {
        tracing::info!(
            "levels not mapped (probe), at {moment}: {} textures of several levels, {} MB of {} MB console bytes, {} MB of {} MB host bytes",
            t.textures,
            t.unheld >> 20,
            t.console >> 20,
            t.host_unheld >> 20,
            t.host >> 20,
        );
    }
}

/// Whether the samples are on (`VICTORY_PROBE_LEVELS=1`; the resident one asks
/// the kernel about every level of every live host texture).
pub(crate) fn sampled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_PROBE_LEVELS").is_ok_and(|v| v == "1"))
}

/// With the host textures line: of the resident host textures made for
/// title memory, the host bytes in levels the title has not mapped now,
/// and how many of those textures hold no level at all.
pub(crate) fn sample() {
    if !sampled() {
        return;
    }
    let started = std::time::Instant::now();
    let (mut textures, mut host, mut unheld, mut empty) = (0u64, 0u64, 0u64, 0u64);
    for t in crate::resource::titled() {
        let Some((address, layout)) = t.title.get() else { continue };
        if !t.resident() || layout.mips.is_empty() {
            continue;
        }
        let spans = spans(layout);
        let held = held(*address as *const u8, layout);
        let mut seen = std::collections::BTreeSet::new();
        let (mut all, mut not) = (0u64, 0u64);
        for (&(start, end), &h) in spans.iter().zip(&held) {
            if seen.insert(start) {
                all += end - start;
                if !h {
                    not += end - start;
                }
            }
        }
        textures += 1;
        host += t.bytes();
        if all > 0 {
            unheld += (u128::from(t.bytes()) * u128::from(not) / u128::from(all)) as u64;
        }
        if held.iter().all(|&h| !h) {
            empty += 1;
        }
    }
    tracing::info!(
        "levels not mapped (probe), resident now: {textures} textures, {} MB of {} MB host bytes; {empty} hold no level; sampled in {:.1} ms",
        unheld >> 20,
        host >> 20,
        started.elapsed().as_secs_f64() * 1e3,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use victory_formats::texture::{Desc, Dimension, Swizzle};

    fn layout(swizzle: u32, mips: u32) -> Layout {
        Layout::new(&Desc {
            dimension: Dimension::Tex2d,
            width: 512,
            height: 512,
            depth: 1,
            mips,
            element_bytes: 4,
            block: 1,
            samples: 1,
            swizzle: Swizzle(swizzle),
        })
        .expect("a valid layout")
    }

    #[test]
    fn level_spans_cover_the_slice_without_overlap() {
        // 64 KiB standard swizzle (mode 9) and 4 KiB (mode 5).
        for swizzle in [9, 5] {
            let l = layout(swizzle, 10);
            let spans = spans(&l);
            assert_eq!(spans.len(), 10);
            let mut distinct: Vec<_> = spans.clone();
            distinct.sort_unstable();
            distinct.dedup();
            let covered: u64 = distinct.iter().map(|(s, e)| e - s).sum();
            assert_eq!(covered, l.slice_size, "mode {swizzle}");
            for w in distinct.windows(2) {
                assert!(w[0].1 <= w[1].0, "mode {swizzle}: {w:?}");
            }
            // The largest level is the largest span.
            let biggest = spans.iter().map(|(s, e)| e - s).max();
            assert_eq!(Some(spans[0].1 - spans[0].0), biggest, "mode {swizzle}");
        }
    }

    #[test]
    fn a_level_counts_held_only_where_its_memory_is_mapped() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
        let l = layout(9, 10);
        let spans = spans(&l);
        // SAFETY: a reserved range for the test, freed below.
        let base = unsafe { VirtualAlloc(std::ptr::null(), l.slice_size as usize, MEM_RESERVE, PAGE_READWRITE) };
        assert!(!base.is_null());
        // Commit everything but the largest level.
        // SAFETY: inside the reservation.
        let c = unsafe { VirtualAlloc(base, spans[0].0 as usize, MEM_COMMIT, PAGE_READWRITE) };
        assert!(!c.is_null());
        let held = held(base.cast(), &l);
        assert!(!held[0]);
        assert!(held[1..].iter().all(|&h| h), "{held:?}");
        // SAFETY: freeing the reservation above.
        unsafe { VirtualFree(base, 0, MEM_RELEASE) };
    }
}
