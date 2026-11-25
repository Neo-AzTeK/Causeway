//! Host copies of title buffers (decision 0007, step 3 and "Copies of title
//! memory"). Title buffers are title memory, which the host GPU cannot
//! read, so a buffer a draw uses gets a persistent host buffer of its size
//! on first use (system memory shaders may also write), and the ranges each
//! submission reads (vertex, index and constant data, buffer views) are
//! brought up to date from title memory before it runs: the 64 KiB pages
//! the title wrote since they were last copied ([`crate::watch`]: write
//! watching, or read-only guards on physical-page views), all of it where
//! neither works. A title GPU address inside the buffer is the same offset
//! in its copy. Ranges shaders write go back to title memory after the
//! submission ([`Shadow::gpu_writes`]).
//!
//! shortcut: only the ranges draws are known to read are brought up to
//! date (vertex and index buffer views, constant buffers in tables, root
//! descriptors, buffer views in tables); buffers read through directly
//! indexed heaps keep what their first use copied.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

use crate::host::{HostList, host};
use crate::watch::PAGE;

/// A title buffer's host copy.
pub(crate) struct Shadow {
    pub(crate) buffer: ID3D12Resource,
    data: *mut u8,
    gpu: u64,
    /// Where the title buffer starts and how long it is.
    start: u64,
    len: u64,
    /// Per page of title memory from `start`'s on: the write-watch asking
    /// after which it was last copied (0: never).
    copied: Mutex<Vec<u64>>,
    /// Whether the memory is write-watched: unknown, yes, no.
    watched: AtomicU8,
    /// Ranges shaders of a submission not yet copied back may be writing:
    /// title memory there is older than the copy.
    pending: Mutex<Vec<(usize, usize)>>,
    /// The copy's bytes before the submission ran, from an offset on,
    /// covering the ranges its shaders write: what the GPU wrote differs.
    before: Mutex<Option<(usize, Vec<u8>)>>,
}

/// Snapshot memory kept for reuse (already touched, so no page faults).
static POOL: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// An empty buffer with room for `n` bytes, from the pool if it has one.
fn pooled(n: usize) -> Vec<u8> {
    let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
    let fit = pool
        .iter()
        .enumerate()
        .filter(|(_, v)| v.capacity() >= n)
        .min_by_key(|(_, v)| v.capacity())
        .map(|(i, _)| i);
    let mut v = match fit {
        Some(i) => pool.swap_remove(i),
        None => Vec::with_capacity(n),
    };
    v.clear();
    v
}

/// Returns a snapshot buffer to the pool (at most 8 kept, the largest).
fn give_back(v: Vec<u8>) {
    let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
    pool.push(v);
    if pool.len() > 8 {
        pool.sort_by_key(|v| std::cmp::Reverse(v.capacity()));
        pool.truncate(8);
    }
}

const NOT_WATCHED: u8 = 2;

// SAFETY: D3D12 resources are free-threaded; `data` is a persistent mapping
// written only on the submitting thread.
unsafe impl Send for Shadow {}
// SAFETY: as above.
unsafe impl Sync for Shadow {}

impl Shadow {
    /// The host GPU address of a title address inside the buffer.
    pub(crate) fn gpu(&self, address: u64) -> u64 {
        self.gpu + (address - self.start)
    }

    /// Bytes from a title address to the buffer's end.
    pub(crate) fn left(&self, address: u64) -> u64 {
        (self.start + self.len).saturating_sub(address)
    }

    /// Offset of a title address from the buffer's start.
    pub(crate) fn offset(&self, address: u64) -> u64 {
        address - self.start
    }

    /// Brings `len` bytes at `address` up to date when `list` is submitted,
    /// and keeps the copy alive until the list is reset.
    pub(crate) fn refresh(self: &Arc<Self>, list: &HostList, address: u64, len: u64) {
        let len = len.min(self.start + self.len - address);
        list.defer_refresh(self, address as usize, len as usize);
    }

    fn new(buffer: ID3D12Resource, data: *mut u8, gpu: u64, start: u64, len: u64) -> Shadow {
        let pages = (start + len).div_ceil(PAGE as u64) - start / PAGE as u64;
        Shadow {
            buffer,
            data,
            gpu,
            start,
            len,
            copied: Mutex::new(vec![0; pages as usize]),
            watched: AtomicU8::new(0),
            pending: Mutex::default(),
            before: Mutex::default(),
        }
    }

    /// `[address, address + len)` cut to the buffer, as offsets in the copy.
    fn inside(&self, address: usize, len: usize) -> (usize, usize) {
        let (start, end) = (self.start as usize, (self.start + self.len) as usize);
        let from = address.clamp(start, end);
        (from - start, (address + len).clamp(from, end) - start)
    }

    /// Shaders of `list` may write `[address, address + len)` of the copy
    /// (unordered access): after the list's submission has run, the range
    /// is copied back into title memory, where the title reads it; until
    /// then no refresh copies title memory over it. Call after the
    /// [`Shadow::refresh`] that gives the shaders the title's data.
    ///
    /// shortcut: what shaders wrote is found by keeping the range's bytes
    /// before the submission (a copy per declared range and submission);
    /// a GPU-side dirty mask would save that if big ranges make it slow.
    pub(crate) fn gpu_writes(self: &Arc<Self>, list: &HostList, address: u64, len: u64) {
        let len = len.min(self.left(address));
        list.defer_gpu_write(self, address as usize, len as usize);
    }

    /// At submission, after the range's refresh: a submitted list's shaders
    /// write this range. Its bytes are kept to tell later what the GPU wrote:
    /// one snapshot per copy and submission, grown to cover every range the
    /// submission declares (the world declares overlapping views of its
    /// 174 MB heap dozens of times), in reused memory (fresh snapshots cost
    /// ~2.3 million page faults per world frame).
    pub(crate) fn writing(&self, address: usize, len: usize) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((address, address + len));
        let (from, to) = self.inside(address, len);
        // SAFETY: inside the mapping.
        let host = |a: usize, b: usize| unsafe { std::slice::from_raw_parts(self.data.add(a), b - a) };
        let mut before = self.before.lock().unwrap_or_else(|e| e.into_inner());
        let (start, end) = match &*before {
            Some((at, bytes)) => ((*at).min(from), (at + bytes.len()).max(to)),
            None => (from, to),
        };
        if let Some((at, bytes)) = &*before
            && (*at, at + bytes.len()) == (start, end)
        {
            return;
        }
        // Parts newly covered from the copy, the rest as it was taken.
        let mut grown = pooled(end - start);
        match before.take() {
            Some((at, bytes)) => {
                grown.extend_from_slice(host(start, at));
                grown.extend_from_slice(&bytes);
                grown.extend_from_slice(host(at + bytes.len(), end));
                give_back(bytes);
            }
            None => grown.extend_from_slice(host(start, end)),
        }
        *before = Some((start, grown));
    }

    /// After the submission ran: copies the bytes its shaders wrote back
    /// into title memory (unmapped pages left alone), and only those: the
    /// title's threads write the same buffers while the GPU runs (its
    /// command memory shares the big GPU heap with what shaders write), and
    /// putting back the copy's older bytes there broke the title's own
    /// records (a fence list read as -1, title+0x4e9b54a). `ranges` are all
    /// the ones the submission declared for this copy (address, length):
    /// overlapping ones are joined and each byte is compared once (the world
    /// declares dozens of overlapping views of its 174 MB heap, which took
    /// seconds a frame when compared one by one). Returns the bytes written.
    pub(crate) fn copy_back(&self, ranges: &[(usize, usize)]) -> usize {
        let mut snapshot = self.before.lock().unwrap_or_else(|e| e.into_inner());
        let mut written = 0;
        let start = self.start as usize;
        let cut: Vec<(usize, usize)> = ranges
            .iter()
            .map(|&(address, len)| self.inside(address, len))
            .collect();
        for (from, to) in join(cut) {
            let Some((taken, before)) = &mut *snapshot else {
                break;
            };
            if !(*taken <= from && to <= *taken + before.len()) {
                continue;
            }
            // By 64 KiB piece: the copy compared with its bytes before the
            // submission, title memory read only where the GPU wrote.
            const PIECE: usize = 64 << 10;
            let mut title = vec![0u8; PIECE];
            let mut synced = Vec::new();
            let mut at = from;
            while at < to {
                let n = (PIECE - (start + at) % PIECE).min(to - at);
                // SAFETY: inside the mapping; only the submitting thread
                // writes it.
                let host = unsafe { std::slice::from_raw_parts_mut(self.data.add(at), n) };
                let old = &mut before[at - *taken..at - *taken + n];
                let title = &mut title[..n];
                #[cfg(feature = "diagnostics")]
                let t0 = std::time::Instant::now();
                let differs = host != old;
                #[cfg(feature = "diagnostics")]
                crate::copystat::add(crate::copystat::C::BackCompareUs, t0.elapsed().as_micros() as u64);
                #[cfg(feature = "diagnostics")]
                let t1 = std::time::Instant::now();
                let readable = differs && crate::host::read_into((start + at) as *const u8, title);
                #[cfg(feature = "diagnostics")]
                if differs {
                    use crate::copystat::{C, add};
                    add(C::BackReadUs, t1.elapsed().as_micros() as u64);
                    add(C::BackDiffBytes, n as u64);
                }
                if readable {
                    // Title memory with the GPU's bytes put in; the copy
                    // takes the title's bytes in turn, and so does the
                    // snapshot: a later look finds nothing new.
                    #[cfg(feature = "diagnostics")]
                    let t2 = std::time::Instant::now();
                    let changed = merge_gpu(title, host, old);
                    #[cfg(feature = "diagnostics")]
                    crate::copystat::add(crate::copystat::C::BackMergeUs, t2.elapsed().as_micros() as u64);
                    if changed {
                        #[cfg(feature = "diagnostics")]
                        let t3 = std::time::Instant::now();
                        crate::watch::writing(start + at, n);
                        crate::host::write_mapped((start + at) as *mut u8, title.as_ptr(), n);
                        #[cfg(feature = "diagnostics")]
                        crate::copystat::add(crate::copystat::C::BackWriteUs, t3.elapsed().as_micros() as u64);
                        written += n;
                    }
                    synced.push((start + at) / PAGE);
                }
                at += n;
            }
            // Pages made equal to title memory are current in the copy, so
            // our own write is not taken for the title's; others keep what
            // they had (a title write there shows at the next refresh).
            let (first, last) = ((start + from).div_ceil(PAGE), (start + to) / PAGE);
            #[cfg(feature = "diagnostics")]
            let t4 = std::time::Instant::now();
            if first < last
                && !synced.is_empty()
                && self.watched.load(Ordering::Relaxed) != NOT_WATCHED
                && let Some((now, _)) = crate::watch::writes(first * PAGE, last * PAGE, true)
            {
                let base = start / PAGE;
                let mut copied = self.copied.lock().unwrap_or_else(|e| e.into_inner());
                for page in synced.into_iter().filter(|p| (first..last).contains(p)) {
                    copied[page - base] = now;
                }
            }
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::BackWatchUs, t4.elapsed().as_micros() as u64);
        }
        // Every submitted step that declared the range is done with it (one
        // submission runs at a time), and the snapshot with the last range.
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|&(a, b)| !ranges.contains(&(a, b - a)));
        if pending.is_empty()
            && let Some((_, bytes)) = snapshot.take()
        {
            give_back(bytes);
        }
        written
    }

    /// At submission: copies what changed of `[address, address + len)`
    /// since it was last copied (all of it if the memory is not
    /// write-watched). Returns the bytes copied.
    pub(crate) fn update(&self, address: usize, len: usize) -> usize {
        let (start, end) = (self.start as usize, (self.start + self.len) as usize);
        let (from, to) = (address.max(start), (address + len).min(end));
        if from >= to {
            return 0;
        }
        let mut busy: Vec<(usize, usize)> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .filter(|&(a, e)| a < to && e > from)
            .collect();
        if !busy.is_empty() {
            // Around ranges shaders are writing: the gaps, each as below
            // (in the world most submissions have such ranges; copying the
            // gaps whole cost ~1 s per frame).
            busy.sort_unstable();
            let (mut at, mut bytes) = (from, 0);
            for (a, e) in busy {
                if a > at {
                    bytes += self.update_free(at, a);
                }
                at = at.max(e);
            }
            if at < to {
                bytes += self.update_free(at, to);
            }
            return bytes;
        }
        self.update_free(from, to)
    }

    /// [`Shadow::update`] of `[from, to)` (inside the buffer) where no
    /// shader writes.
    fn update_free(&self, from: usize, to: usize) -> usize {
        if self.watched.load(Ordering::Relaxed) != NOT_WATCHED {
            if let Some((now, written)) = crate::watch::writes(from, to, true) {
                self.watched.store(1, Ordering::Relaxed);
                let copied = self.copy_stale(from, to, now, &written);
                #[cfg(feature = "diagnostics")]
                {
                    use crate::copystat::{C, add};
                    add(C::Checked, (to - from) as u64);
                    add(C::Unchanged, (to - from).saturating_sub(copied) as u64);
                }
                return copied;
            }
            self.watched.store(NOT_WATCHED, Ordering::Relaxed);
        }
        self.copy(from, to);
        to - from
    }

    /// Copies the pages of `[from, to)` written after their last copy
    /// (`written`: last writes by page, in page order; `now`: this
    /// asking), whole pages within the buffer.
    fn copy_stale(&self, from: usize, to: usize, now: u64, written: &[(usize, u64)]) -> usize {
        let base = self.start as usize / PAGE;
        let mut copied = self.copied.lock().unwrap_or_else(|e| e.into_inner());
        let mut writes = written.iter().peekable();
        let (mut bytes, mut run) = (0, None);
        let last = (to - 1) / PAGE;
        for page in from / PAGE..=last {
            let write = match writes.peek() {
                Some(&&(p, w)) if p == page => {
                    writes.next();
                    w
                }
                _ => 0,
            };
            let at = &mut copied[page - base];
            if *at == 0 || write > *at {
                // A page only partly in the range is copied in part: it
                // stays stale for the rest.
                let (start, end) = (self.start as usize, (self.start + self.len) as usize);
                if (page * PAGE).max(start) >= from && ((page + 1) * PAGE).min(end) <= to {
                    *at = now;
                }
                run.get_or_insert(page);
            } else if let Some(first) = run.take() {
                bytes += self.copy((first * PAGE).max(from), (page * PAGE).min(to));
            }
        }
        // Cut to the range: next to it shaders may be writing the copy.
        if let Some(first) = run {
            bytes += self.copy((first * PAGE).max(from), ((last + 1) * PAGE).min(to));
        }
        bytes
    }

    /// Copies title memory `[from, to)`, cut to the buffer, into the copy.
    fn copy(&self, from: usize, to: usize) -> usize {
        let (start, end) = (self.start as usize, (self.start + self.len) as usize);
        let (from, to) = (from.max(start), to.min(end));
        // SAFETY: inside the mapping (`from` and `to` are within the buffer).
        let at = unsafe { self.data.add(from - start) };
        crate::host::copy_mapped(at, from as *const u8, to - from, false);
        to - from
    }
}

struct Entry {
    end: u64,
    shadow: Option<Arc<Shadow>>,
}

/// Title buffers by (start address, resource), with their copy once made.
static BUFFERS: Mutex<BTreeMap<(u64, usize), Entry>> = Mutex::new(BTreeMap::new());

fn buffers() -> std::sync::MutexGuard<'static, BTreeMap<(u64, usize), Entry>> {
    BUFFERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A title buffer exists at `start` (`len` bytes).
pub(crate) fn register(resource: usize, start: u64, len: u64) {
    buffers().insert(
        (start, resource),
        Entry {
            end: start + len,
            shadow: None,
        },
    );
}

/// The title released it; lists that still hold its copy keep it alive.
pub(crate) fn unregister(resource: usize, start: u64) {
    buffers().remove(&(start, resource));
}

/// The copy of the innermost title buffer holding `[address, address +
/// len)`, made on first use.
pub(crate) fn find(address: u64, len: u64) -> Option<Arc<Shadow>> {
    let end = address.checked_add(len.max(1))?;
    let mut map = buffers();
    let key = map
        .range(..=(address, usize::MAX))
        .rev()
        .take(64)
        .find(|(_, e)| e.end >= end)
        .map(|(k, _)| *k);
    let Some(key) = key else {
        drop(map);
        return loose(address, end);
    };
    let entry = map.get_mut(&key)?;
    if entry.shadow.is_none() {
        let len = entry.end - key.0;
        // Rounded up so that views ending in the buffer's last 256 bytes fit.
        // Shaders write some buffers (unordered access), so not upload memory.
        let (buffer, data) = host()?.shared_buffer(len.next_multiple_of(1 << 16))?;
        crate::host::name(&buffer, || format!("title buffer {:#x}+{len:#x}", key.0));
        // SAFETY: a live buffer.
        let gpu = unsafe { buffer.GetGPUVirtualAddress() };
        if len >= 16 << 20 {
            tracing::info!(
                start = format_args!("{:#x}", key.0),
                len,
                "large buffer copied to the host"
            );
        } else {
            tracing::debug!(
                start = format_args!("{:#x}", key.0),
                len,
                "buffer copied to the host"
            );
        }
        entry.shadow = Some(Arc::new(Shadow::new(buffer, data, gpu, key.0, len)));
    }
    entry.shadow.clone()
}

/// The copy of `[address, address + len)` for a view of `element`-byte
/// elements, which starts on a whole element of the copy: the buffer's, or
/// one whose windows start on a multiple of the element from `address`.
/// shortcut: what shaders write in one of two copies of the same memory
/// reaches the other through title memory, after the submission. Copies
/// that start on an element are kept for good, up to [`ALIGNED`] bytes in
/// all; views past that are not made. Like the other windows, one is brought
/// up to date where the memory is written: a write-watched page freed, given
/// out again and not written since keeps the bytes it had.
pub(crate) fn find_element(address: u64, len: u64, element: u64) -> Option<Arc<Shadow>> {
    let shadow = find(address, len)?;
    if element == 0 || shadow.offset(address).is_multiple_of(element) {
        return Some(shadow);
    }
    let unit = WINDOW / element * element;
    if unit == 0 {
        return None;
    }
    let start = address - (address - address % element) % unit;
    window(start, address.checked_add(len.max(1))?, unit, true)
}

/// Titles also use memory outside any buffer object as GPU memory (Grand
/// Theft Auto VI's interface vertices, from its own allocator). Such memory
/// gets copies of the aligned windows it touches.
/// shortcut: copies of windows are never freed; key them to the title's
/// allocations if memory use grows.
const WINDOW: u64 = 4 << 20;

/// Copies of title memory outside buffers, by (start, length).
static LOOSE: Mutex<BTreeMap<(u64, u64), Arc<Shadow>>> = Mutex::new(BTreeMap::new());

fn loose(address: u64, end: u64) -> Option<Arc<Shadow>> {
    window(address & !(WINDOW - 1), end, WINDOW, false)
}

/// The most bytes of copies that start on an element, and those made.
const ALIGNED: u64 = 256 << 20;
static ALIGNED_MADE: AtomicU64 = AtomicU64::new(0);

/// Counts `bytes` more into `made` unless that passes `most`.
fn reserve(made: &AtomicU64, bytes: u64, most: u64) -> bool {
    made.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        n.checked_add(bytes).filter(|&n| n <= most)
    })
    .is_ok()
}

/// The copy of the windows of `unit` bytes from `start` that reach `end`;
/// `aligned`: one of those [`ALIGNED`] bounds.
fn window(start: u64, end: u64, unit: u64, aligned: bool) -> Option<Arc<Shadow>> {
    let len = (end - start).next_multiple_of(unit);
    let mut map = LOOSE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(shadow) = map.get(&(start, len)) {
        return Some(shadow.clone());
    }
    if aligned && !reserve(&ALIGNED_MADE, len, ALIGNED) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!(len, most = ALIGNED, "no more element-aligned copies"));
        return None;
    }
    let (buffer, data) = host()?.shared_buffer(len)?;
    crate::host::name(&buffer, || format!("title memory {start:#x}+{len:#x}"));
    // SAFETY: a live buffer.
    let gpu = unsafe { buffer.GetGPUVirtualAddress() };
    tracing::debug!(
        start = format_args!("{start:#x}"),
        len,
        "loose memory copied to the host"
    );
    let shadow = Arc::new(Shadow::new(buffer, data, gpu, start, len));
    map.insert((start, len), shadow.clone());
    Some(shadow)
}

/// For device-removed reports: the copy of title memory whose host range
/// starts closest below host GPU address `va`, and where `va` is in it.
pub(crate) fn near(va: u64) -> String {
    let mut all: Vec<(u64, u64, u64)> = buffers()
        .values()
        .filter_map(|e| e.shadow.as_deref().map(|s| (s.gpu, s.start, s.len)))
        .collect();
    all.extend(
        LOOSE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|s| (s.gpu, s.start, s.len)),
    );
    let below = all.iter().filter(|c| c.0 <= va).max();
    let above = all.iter().filter(|c| c.0 > va).min();
    let mut text = match below {
        Some(&(gpu, start, len)) => format!(
            "; the nearest title memory copy below it: {start:#x}+{len:#x} at host {gpu:#x}, offset {:#x}",
            va - gpu
        ),
        None => "; no title memory copy below it".into(),
    };
    if let Some(&(gpu, start, len)) = above {
        text.push_str(&format!(
            "; the nearest above: {start:#x}+{len:#x} at host {gpu:#x} ({} copies in all)",
            all.len()
        ));
    }
    text
}

/// `ranges` (`[from, to)`) with overlapping and touching ones joined, in
/// order; empty ones go.
fn join(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.retain(|&(a, b)| a < b);
    ranges.sort_unstable();
    let mut joined: Vec<(usize, usize)> = Vec::new();
    for (a, b) in ranges {
        match joined.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => joined.push((a, b)),
        }
    }
    joined
}

/// One piece after a submission: `host` is the copy now, `old` what it held
/// before, `title` title memory now. Puts the bytes the GPU changed
/// (`host != old`) into `title` where the title has not changed them too,
/// and makes `host` and `old` equal to `title`. Whether `title` changed.
fn merge_gpu(title: &mut [u8], host: &mut [u8], old: &mut [u8]) -> bool {
    // By 8 bytes: most of a piece the GPU did not write is equal in `host`
    // and `old`, and takes `title` whole; the branchy per-byte loop below
    // runs only in words that differ.
    let mut changed = false;
    let words = title.len() / 8 * 8;
    let (tw, tr) = title.split_at_mut(words);
    let (hw, hr) = host.split_at_mut(words);
    let (ow, or) = old.split_at_mut(words);
    let (tw, hw, ow) = (tw.as_chunks_mut::<8>().0, hw.as_chunks_mut::<8>().0, ow.as_chunks_mut::<8>().0);
    for ((t, h), o) in tw.iter_mut().zip(hw.iter_mut()).zip(ow.iter_mut()) {
        if h == o {
            *h = *t;
            *o = *t;
        } else {
            changed |= merge_bytes(t, h, o);
        }
    }
    changed |= merge_bytes(tr, hr, or);
    changed
}

fn merge_bytes(title: &mut [u8], host: &mut [u8], old: &mut [u8]) -> bool {
    let mut changed = false;
    for ((t, h), o) in title.iter_mut().zip(host.iter_mut()).zip(old.iter_mut()) {
        if *h != *o && *t != *h {
            *t = *h;
            changed = true;
        }
        *h = *t;
        *o = *t;
    }
    changed
}

/// The per-byte version, for comparison in tests.
#[cfg(test)]
fn merge_gpu_bytes(title: &mut [u8], host: &mut [u8], old: &mut [u8]) -> bool {
    merge_bytes(title, host, old)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Overlapping and touching ranges become one; empty ones go.
    #[test]
    fn ranges_are_joined() {
        assert_eq!(join(vec![(10, 20), (0, 5), (5, 8), (15, 30), (40, 40)]), [(0, 8), (10, 30)]);
    }

    fn sample(n: usize, seed: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut s = seed;
        let mut next = move || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            (s >> 24) as u8
        };
        let old: Vec<u8> = (0..n).map(|_| next()).collect();
        // The GPU wrote ~1% of the bytes, the title ~1% (of other ones, mostly).
        let host = old.iter().map(|&b| if next() < 3 { b ^ 0x55 } else { b }).collect();
        let title = old.iter().map(|&b| if next() < 3 { b ^ 0xaa } else { b }).collect();
        (title, host, old)
    }

    /// The word-wise merge gives what the per-byte one does, for lengths
    /// that are not multiples of 8 too.
    #[test]
    fn word_merge_matches_bytes() {
        for n in [0, 1, 7, 8, 9, 100, 4099] {
            let (t, h, o) = sample(n, n as u32 + 1);
            let (mut t1, mut h1, mut o1) = (t.clone(), h.clone(), o.clone());
            let (mut t2, mut h2, mut o2) = (t, h, o);
            assert_eq!(merge_gpu(&mut t1, &mut h1, &mut o1), merge_gpu_bytes(&mut t2, &mut h2, &mut o2));
            assert_eq!((t1, h1, o1), (t2, h2, o2));
        }
    }

    /// `cargo test -p victory-gpu --release --lib -- --ignored --nocapture merge_speed`
    #[test]
    #[ignore]
    fn merge_speed() {
        let n = 64 << 20;
        let (t, h, o) = sample(n, 7);
        let time = |f: fn(&mut [u8], &mut [u8], &mut [u8]) -> bool| {
            let (mut t, mut h, mut o) = (t.clone(), h.clone(), o.clone());
            let at = std::time::Instant::now();
            f(&mut t, &mut h, &mut o);
            at.elapsed()
        };
        println!("per byte {:?}, per word {:?} for 64 MiB", time(merge_gpu_bytes), time(merge_gpu));
    }

    /// The GPU's bytes reach title memory unless the title changed them
    /// too; a second look at the same piece finds nothing to do.
    #[test]
    fn gpu_bytes_merge_once() {
        let mut old = vec![1u8, 2, 3, 4];
        let mut host = vec![1u8, 9, 9, 4];
        let mut title = vec![7u8, 2, 8, 4];
        assert!(merge_gpu(&mut title, &mut host, &mut old));
        assert_eq!(title, [7, 9, 9, 4]);
        assert_eq!(host, title);
        assert_eq!(old, title);
        // The title writes on; nothing of the GPU's is left to put back.
        title[0] = 5;
        assert!(host == old);
        assert!(!merge_gpu(&mut title, &mut host, &mut old));
        assert_eq!(title[0], 5);
    }

    /// Element-aligned copies stop at their bound, and one refused leaves
    /// room for a smaller one.
    #[test]
    fn aligned_copies_are_bounded() {
        let made = AtomicU64::new(0);
        assert!(reserve(&made, 6, 10));
        assert!(!reserve(&made, 5, 10));
        assert!(reserve(&made, 4, 10));
        assert!(!reserve(&made, 1, 10));
        assert!(!reserve(&made, u64::MAX, 10));
        // A range past the bound gets no copy.
        assert!(window(16, 16 + ALIGNED + 1, WINDOW, true).is_none());
    }
}
