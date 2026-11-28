//! Which title memory changed since a host copy took it (decision 0007,
//! "Copies of title memory" and "Change tracking"). Each host copy of title
//! memory (`crate::shadow`) and each texture's staging (`crate::texture`)
//! tracks its memory ([`Tracked`]): the 64 KiB pages it has not taken since
//! they last changed. Taking pages makes their blocks read-only (guards);
//! the first write to a guarded block faults into our vectored handler,
//! which gives write access back and marks the block changed in every copy
//! holding it. Writers that do not fault say so: our own and the kernel's
//! writes into title memory ([`writing`], before they write), and the
//! runtime mapping or unmapping memory ([`title_memory_remapped`]). So
//! bringing a copy up to date costs what changed since, not how much memory
//! it covers: no system call for memory nobody wrote.
//!
//! Only the runtime's GPU memory is tracked: its write-watched pools, its
//! physical-page views and their placeholders. Other memory a copy takes
//! (the C heap, where the kernel writes system call results into buffers a
//! guard would make fail) is copied whole every time, also where a tracked
//! range covers it (a loose copy's 4 MiB window around title memory may
//! cover the C heap): a guard there made the heap's own writes fault into
//! our handler, which waits for the guard lock its thread may hold, and
//! made kernel copies into the heap fail.
//!
//! Blocks that cannot be made read-only (placeholders, nothing mapped)
//! read as zeros and count as taken: only a mapping changes them, which the
//! runtime reports.
//!
//! Physical pages may be mapped at several addresses (the title fills
//! pages through a pool view and maps them into its heaps), so a write at
//! one is a change at all of them: the runtime says where else memory is
//! mapped whenever it maps it ([`title_memory_aliased`]); taking a page
//! guards every address showing it, and a write at any of them marks the
//! trackers at all. (Asking the runtime at each take instead waited for
//! its lock, which the title's page mappings hold: 4 s per world frame.)
//! DirectStorage writing pages through views of the runtime's own reports
//! the addresses showing them as written.
//!
//! Pages disk loads left without bytes (`crate::lazy`) are guarded without
//! any access and counted as guarded: an access fills them first
//! ([`fill`]), our reads take their bytes from the load ([`read_lazy`]).
//!
//! shortcut: a system call writing into a guarded block fails; the known
//! ones (file reads, DirectStorage) open their blocks first. Memory freed
//! and allocated again while a tracker holds it is not seen.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The unit tracked and copied: 64 KiB, the console's page. Finer units
/// made the checks of the world's big buffers cost more than the copies
/// they saved.
pub(crate) const PAGE: usize = 64 << 10;

/// The pages a tracker has not taken since they changed, and what it saves
/// of pages it armed, and how many pages the set holds: kept with the set
/// under its lock, read without it, so a check of a tracker with nothing
/// changed costs one load (`docs/memory-design.md`, step 1).
struct Changed(Mutex<BTreeSet<usize>>, Mutex<Saved>, AtomicUsize);

/// Pages (page numbers) whose bytes a tracker keeps when they are first
/// written after it took them, and those bytes: while shaders may be
/// writing its copy of them, what it held before (`crate::shadow`'s
/// copy-back of marked pages).
#[derive(Default)]
struct Saved {
    armed: BTreeSet<usize>,
    bytes: std::collections::HashMap<usize, Box<[u8]>>,
}

impl Changed {
    /// The set changed: its size, stored while its lock is still held so the
    /// count never trails a page a check could miss.
    fn count(&self, set: &BTreeSet<usize>) {
        self.2.store(set.len(), Ordering::Release);
    }

    /// Page `page` was taken: what it saved of it is stale.
    fn taken(&self, page: usize) {
        self.1.lock().unwrap_or_else(|e| e.into_inner()).bytes.remove(&page);
    }

    /// Page `page` is about to change for the first time since it was
    /// taken: its bytes now, if armed for it.
    fn save(&self, page: usize) {
        let mut saved = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if !saved.armed.contains(&page) || saved.bytes.contains_key(&page) {
            return;
        }
        let mut bytes = vec![0u8; PAGE].into_boxed_slice();
        if crate::host::read_into((page * PAGE) as *const u8, &mut bytes) {
            saved.bytes.insert(page, bytes);
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::SavedPages, 1);
        }
    }
}

/// Title memory a host copy or texture staging takes: bytes `[start, end)`.
pub(crate) struct Tracked {
    start: usize,
    end: usize,
    changed: Arc<Changed>,
}

struct Guards {
    /// Blocks (pages) we made read-only.
    protected: BTreeSet<usize>,
    /// The trackers holding each page.
    holders: BTreeMap<usize, Vec<Arc<Changed>>>,
    /// Pages `[first, last)` of writes the guards do not see that are under
    /// way ([`Writing`]): not guarded until they end, or the write fails.
    busy: Vec<(usize, usize)>,
    /// Pages showing the same physical pages elsewhere, both ways, as the
    /// runtime told when it mapped them: a write at one marks the others,
    /// with no question to the runtime (whose lock its page mappings hold
    /// for long).
    alias_of: BTreeMap<usize, Vec<usize>>,
    /// Pages left without bytes by disk loads (`crate::lazy`), with no
    /// access, and where in their load each starts. Each is in
    /// `protected` too.
    lazy: BTreeMap<usize, (Arc<crate::lazy::Load>, usize)>,
    /// Tracked pages written this frame and the frame before (diagnostics).
    #[cfg(feature = "diagnostics")]
    written: BTreeSet<usize>,
    #[cfg(feature = "diagnostics")]
    written_before: BTreeSet<usize>,
    /// Pages [`Tracked::take`] gave to copy this frame (diagnostics).
    #[cfg(feature = "diagnostics")]
    copied: BTreeSet<usize>,
}

static GUARDS: Mutex<Guards> = Mutex::new(Guards {
    protected: BTreeSet::new(),
    holders: BTreeMap::new(),
    busy: Vec::new(),
    alias_of: BTreeMap::new(),
    lazy: BTreeMap::new(),
    #[cfg(feature = "diagnostics")]
    written: BTreeSet::new(),
    #[cfg(feature = "diagnostics")]
    written_before: BTreeSet::new(),
    #[cfg(feature = "diagnostics")]
    copied: BTreeSet::new(),
});

#[cfg(not(feature = "diagnostics"))]
fn guards() -> std::sync::MutexGuard<'static, Guards> {
    GUARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The guard lock, timed while held by the line that took it
/// (diagnostics: [`held_times`]).
#[cfg(feature = "diagnostics")]
#[track_caller]
fn guards() -> Held {
    let line = std::panic::Location::caller().line();
    let guard = GUARDS.lock().unwrap_or_else(|e| e.into_inner());
    Held(Some(guard), std::time::Instant::now(), line)
}

#[cfg(feature = "diagnostics")]
struct Held(Option<std::sync::MutexGuard<'static, Guards>>, std::time::Instant, u32);

#[cfg(feature = "diagnostics")]
impl std::ops::Deref for Held {
    type Target = Guards;
    fn deref(&self) -> &Guards {
        self.0.as_ref().expect("held until dropped")
    }
}

#[cfg(feature = "diagnostics")]
impl std::ops::DerefMut for Held {
    fn deref_mut(&mut self) -> &mut Guards {
        self.0.as_mut().expect("held until dropped")
    }
}

/// Per line taking the guard lock: the line, microseconds held in all, the
/// longest hold (no allocation: the fault handler takes the lock).
#[cfg(feature = "diagnostics")]
static HELD: [[std::sync::atomic::AtomicU64; 3]; 16] =
    [const { [const { std::sync::atomic::AtomicU64::new(0) }; 3] }; 16];

#[cfg(feature = "diagnostics")]
impl Drop for Held {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let us = self.1.elapsed().as_micros() as u64;
        drop(self.0.take());
        let line = u64::from(self.2);
        if let Some(slot) = HELD
            .iter()
            .find(|s| s[0].load(Relaxed) == line || s[0].compare_exchange(0, line, Relaxed, Relaxed).is_ok())
        {
            slot[1].fetch_add(us, Relaxed);
            slot[2].fetch_max(us, Relaxed);
        }
    }
}

/// Since the last call, per line of this file taking the guard lock: (line,
/// milliseconds held, longest hold in milliseconds), most held first.
#[cfg(feature = "diagnostics")]
pub(crate) fn held_times() -> Vec<(u64, f64, f64)> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out: Vec<(u64, f64, f64)> = HELD
        .iter()
        .filter(|s| s[0].load(Relaxed) != 0)
        .map(|s| (s[0].load(Relaxed), s[1].swap(0, Relaxed) as f64 / 1e3, s[2].swap(0, Relaxed) as f64 / 1e3))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// (page, other page) for the pages (page numbers) showing the same memory
/// as `pages` elsewhere.
fn aliases(g: &Guards, pages: &[usize]) -> Vec<(usize, usize)> {
    pages
        .iter()
        .flat_map(|&p| g.alias_of.get(&p).into_iter().flatten().map(move |&a| (p, a)))
        .collect()
}

/// Guards the other mappings of taken pages.
fn guard_aliases(g: &mut Guards, pairs: &[(usize, usize)]) {
    let others: Vec<usize> = pairs.iter().map(|&(_, a)| a).collect();
    protect(g, &others);
}

/// Tracks `[start, start + len)` if it is the runtime's GPU memory (every
/// page changed at first); `None` for other memory, and with `views` false
/// for anything but its write-watched pools (physical-page views are
/// written through other views showing the same pages: the title fills
/// streamed textures and video planes through a pool view and maps the
/// pages into its tiled heap, where a guard sees none of it).
pub(crate) fn track(start: usize, len: usize, views: bool) -> Option<Tracked> {
    if len == 0 || !runtime_memory(start, views) {
        return None;
    }
    install();
    let end = start + len;
    let pages = start / PAGE..end.div_ceil(PAGE);
    let changed = Arc::new(Changed(Mutex::new(pages.clone().collect()), Mutex::default(), AtomicUsize::new(pages.len())));
    let mut g = guards();
    for page in pages {
        g.holders.entry(page).or_default().push(changed.clone());
    }
    Some(Tracked {
        start,
        end,
        changed,
    })
}

/// Whether memory at `at` is the runtime's GPU memory: a write-watched
/// pool, or (with `views`) a view or a placeholder (anything but plain
/// committed memory).
fn runtime_memory(at: usize, views: bool) -> bool {
    use windows_sys::Win32::System::Memory::{
        GetWriteWatch, MEM_COMMIT, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    let page = at & !(4096 - 1);
    let (mut found, mut count, mut granularity) = (0usize, 1usize, 0u32);
    // SAFETY: room for one answer; no reset, so nothing changes.
    let watched = unsafe {
        GetWriteWatch(0, page as *const _, 4096, (&raw mut found).cast(), &mut count, &mut granularity)
    } == 0;
    if watched || !views {
        return watched;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(page as *const _, &mut info, size_of_val(&info)) };
    n != 0 && !(info.State == MEM_COMMIT && info.Type == MEM_PRIVATE)
}

impl Drop for Tracked {
    /// Pages no tracker holds any more get write access back, so that the
    /// guard set never names memory freed and allocated again.
    fn drop(&mut self) {
        let mut g = guards();
        let mut free = Vec::new();
        for page in self.start / PAGE..self.end.div_ceil(PAGE) {
            if let Some(holders) = g.holders.get_mut(&page) {
                holders.retain(|c| !Arc::ptr_eq(c, &self.changed));
                if holders.is_empty() {
                    g.holders.remove(&page);
                    free.push(page);
                }
            }
        }
        for page in free {
            open(&mut g, page);
        }
    }
}

/// Gives write access back to page `page` if we guarded it.
fn open(g: &mut Guards, page: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    // A write is coming: the page takes its load's bytes first.
    if let Some(entry) = g.lazy.remove(&page) {
        #[cfg(feature = "diagnostics")]
        crate::trail::step(page, "filled before a write or an unmap", Some((&entry.0, entry.1)));
        fill(g, page, entry);
    }
    if !g.protected.remove(&page) {
        return false;
    }
    let mut old = 0;
    // SAFETY: a block we made read-only (failing harmlessly if it is gone).
    unsafe { VirtualProtect((page * PAGE) as *const _, PAGE, PAGE_READWRITE, &mut old) };
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::OpenPages, 1);
    true
}

/// Sets the protection of page `page`.
fn set_protection(page: usize, protection: u32) -> bool {
    use windows_sys::Win32::System::Memory::VirtualProtect;
    let mut old = 0;
    // SAFETY: title memory; only its protection changes (failing
    // harmlessly if it is gone).
    unsafe { VirtualProtect((page * PAGE) as *const _, PAGE, protection, &mut old) != 0 }
}

/// Fills page `page`, which a disk load left without bytes and whose entry
/// was just taken out of `lazy`, from that load, leaving it read-write.
/// Addresses showing the same physical page (`alias_of`) left without
/// bytes by the same load now show the bytes too: their entries go, and
/// they are guarded as other pages ([`settle`]).
fn fill(g: &mut Guards, page: usize, entry: (Arc<crate::lazy::Load>, usize)) {
    fill_from(g, page, entry, None);
}

/// [`fill`], with the load's bytes read already where `read` is given
/// (false: they could not be read).
fn fill_from(g: &mut Guards, page: usize, (load, at): (Arc<crate::lazy::Load>, usize), read: Option<(&[u8], bool)>) {
    use windows_sys::Win32::System::Memory::PAGE_READWRITE;
    use crate::ledger::{Owner, add};
    add(Owner::LazyPages, -(PAGE as i64));
    add(Owner::LazyFilled, PAGE as i64);
    // invariant(a_fill_reads_its_bytes_before_the_page_opens): the bytes
    // are read before the page opens: once it is read-write the title's
    // threads use it without faulting, and a disk read or a decode takes
    // milliseconds.
    let mut fetched = Vec::new();
    let (bytes, ok) = match read {
        Some(read) => read,
        None => {
            fetched.resize(PAGE, 0);
            let ok = load.bytes(at, &mut fetched);
            (&fetched[..], ok)
        }
    };
    if !set_protection(page, PAGE_READWRITE) {
        return;
    }
    // SAFETY: a committed page we just made read-write.
    let out = unsafe { std::slice::from_raw_parts_mut((page * PAGE) as *mut u8, PAGE) };
    // shortcut: during this one copy of 64 KiB a title thread reads the
    // page part filled, and its write is copied over (with `fill_unlocked`
    // also not seen by trackers until `settle`); writing the page while it
    // stays without access would need a second address showing it.
    if ok {
        out.copy_from_slice(bytes);
    } else {
        #[cfg(feature = "diagnostics")]
        crate::trail::step(page, "filled with zeros: the load could not be read", Some((&load, at)));
        tracing::warn!(page = format_args!("{:#x}", page * PAGE), "a disk load's bytes could not be read again: zeros");
        out.fill(0);
    }
    let others = g.alias_of.get(&page).cloned().unwrap_or_default();
    for other in others {
        if g.lazy.get(&other).is_some_and(|(l, a)| Arc::ptr_eq(l, &load) && *a == at) {
            #[cfg(feature = "diagnostics")]
            crate::trail::step(other, "filled at another address", Some((&load, at)));
            g.lazy.remove(&other);
            add(Owner::LazyPages, -(PAGE as i64));
            settle(g, other);
        }
    }
}

/// After [`fill`] outside a write: a page trackers hold stays guarded
/// (read-only), others become plain read-write memory.
fn settle(g: &mut Guards, page: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE};
    if g.holders.contains_key(&page) {
        set_protection(page, PAGE_READONLY);
    } else {
        g.protected.remove(&page);
        set_protection(page, PAGE_READWRITE);
    }
}

/// A disk load (`load`) leaves the title page at `at` without bytes; its
/// bytes are `[offset, offset + PAGE)` of the load. What trackers hold of
/// the page is stale, as before a write. False if the page cannot be left
/// so (a write under way, not committed, or the protection failed): the
/// caller writes it then. The runtime calls it for every address showing
/// the page.
pub fn title_memory_lazy(load: &Arc<crate::lazy::Load>, at: usize, offset: usize) -> bool {
    use windows_sys::Win32::System::Memory::{DiscardVirtualMemory, PAGE_NOACCESS};
    // Only the runtime's GPU memory: its views, and its write-watched
    // pools (the C heap's own accesses must never fault here). Asked
    // without `VirtualQuery`, which walks a whole view ([`allocation`]).
    let ours = match allocation(at) {
        Some((_, false)) => true,
        Some((_, true)) => runtime_memory(at, false),
        None => false,
    };
    if !at.is_multiple_of(PAGE) || !ours {
        return false;
    }
    // The handler fills it on access.
    install();
    let page = at / PAGE;
    let mut g = guards();
    if g.busy.iter().any(|&(a, b)| (a..b).contains(&page)) {
        return false;
    }
    // Loaded over again: the older load's bytes are not wanted.
    if g.lazy.remove(&page).is_some() {
        crate::ledger::add(crate::ledger::Owner::LazyPages, -(PAGE as i64));
    }
    changed_here(&mut g, page, page + 1, true, true);
    // What the page held goes back to the system; the protection below
    // keeps it from being read before it is filled.
    // SAFETY: a committed title page whose bytes are replaced.
    unsafe { DiscardVirtualMemory(at as *mut _, PAGE) };
    // Fails where nothing is committed: the caller writes it then.
    if !set_protection(page, PAGE_NOACCESS) {
        return false;
    }
    g.protected.insert(page);
    g.lazy.insert(page, (load.clone(), offset));
    #[cfg(feature = "diagnostics")]
    crate::trail::step(page, "left without bytes", Some((load, offset)));
    crate::ledger::add(crate::ledger::Owner::LazyPages, PAGE as i64);
    true
}

/// What memory `at` is, as [`title_memory_lazy`] tells it apart: a view,
/// a write-watched pool, other private memory, or not answered.
pub fn memory_kind(at: usize) -> &'static str {
    match allocation(at) {
        Some((_, false)) => "view",
        Some((_, true)) if runtime_memory(at, false) => "pool",
        Some((_, true)) => "plain",
        None => "unknown",
    }
}

/// The runtime releases the title reservation holding `at`: pages disk
/// loads left without bytes there go without being filled, and are no
/// longer guarded (the addresses may hold other memory later).
pub fn title_memory_freed(at: usize) {
    let Some((end, _)) = allocation(at) else {
        return;
    };
    let mut g = guards();
    let pages: Vec<usize> = g.lazy.range(at / PAGE..end.div_ceil(PAGE)).map(|(&p, _)| p).collect();
    for page in pages {
        #[cfg(feature = "diagnostics")]
        crate::trail::step(page, "freed without a fill", None);
        g.lazy.remove(&page);
        g.protected.remove(&page);
        crate::ledger::add(crate::ledger::Owner::LazyPages, -(PAGE as i64));
    }
}

/// Reads title memory `[src, src + len)` into `dst` where disk loads left
/// pages without bytes, taking those from their loads and the rest from
/// memory: what a kernel read that stopped at such a page reads. False if
/// no page of the range was left so, or a piece cannot be read.
pub(crate) fn read_lazy(src: usize, dst: *mut u8, len: usize) -> bool {
    let entries: Vec<(usize, Arc<crate::lazy::Load>, usize)> = {
        let g = guards();
        if g.lazy.is_empty() {
            return false;
        }
        g.lazy
            .range(src / PAGE..src.saturating_add(len).div_ceil(PAGE))
            .map(|(&p, (l, a))| (p, l.clone(), *a))
            .collect()
    };
    if entries.is_empty() {
        return false;
    }
    let (mut at, end) = (src, src + len);
    while at < end {
        let page = at / PAGE;
        let to = ((page + 1) * PAGE).min(end);
        // SAFETY: the caller's destination holds `len` bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(dst.add(at - src), to - at) };
        let read = match entries.iter().find(|e| e.0 == page) {
            Some((_, load, offset)) => load.bytes(offset + at % PAGE, out),
            None => crate::host::read_kernel(at as *const u8, out.as_mut_ptr(), out.len()),
        };
        if !read {
            return false;
        }
        at = to;
    }
    true
}

/// Whether a page of `[at, at + len)` is one a disk load left without
/// bytes (not filled yet).
pub(crate) fn lazy_within(at: usize, len: usize) -> bool {
    if crate::ledger::bytes(crate::ledger::Owner::LazyPages) <= 0 {
        return false;
    }
    guards().lazy.range(at / PAGE..at.saturating_add(len).div_ceil(PAGE)).next().is_some()
}

/// Fills the pages of `[at, at + len)` disk loads left without bytes: the
/// GPU is about to read that memory where it lies (copies used in place,
/// `crate::inplace`).
pub(crate) fn fill_lazy(at: usize, len: usize) {
    if crate::ledger::bytes(crate::ledger::Owner::LazyPages) <= 0 {
        return;
    }
    let pages: Vec<usize> = {
        let g = guards();
        g.lazy.range(at / PAGE..at.saturating_add(len).div_ceil(PAGE)).map(|(&p, _)| p).collect()
    };
    for page in pages {
        fill_unlocked(page);
    }
}

/// Fills page `page` if a disk load left it without bytes, reading the
/// load's bytes without the guard lock: a read may wait on the disk or
/// decode a whole load, and every guarded write in the process waits on
/// that lock. Until the page is filled it stays without access, so threads
/// touching it fault and fill it too; whichever finishes first writes it.
/// False if the page was not left without bytes.
fn fill_unlocked(page: usize) -> bool {
    loop {
        let Some((load, at)) = guards().lazy.get(&page).cloned() else {
            return false;
        };
        let mut bytes = vec![0u8; PAGE];
        let ok = load.bytes(at, &mut bytes);
        let mut g = guards();
        match g.lazy.get(&page) {
            // Filled by another thread meanwhile.
            None => return true,
            Some((l, a)) if Arc::ptr_eq(l, &load) && *a == at => {}
            // Loaded over again meanwhile: read that load.
            Some(_) => continue,
        }
        g.lazy.remove(&page);
        #[cfg(feature = "diagnostics")]
        crate::trail::step(page, "filled on access", Some((&load, at)));
        fill_from(&mut g, page, (load, at), Some((&bytes, ok)));
        settle(&mut g, page);
        return true;
    }
}

impl Tracked {
    /// Takes the changed pages of `[from, to)`: guards them (writes from
    /// now on mark them again) and returns the byte ranges to copy, cut to
    /// `[from, to)`, in order. A page whose part in the tracked memory lies
    /// only partly in `[from, to)` is copied in part and stays changed.
    pub(crate) fn take(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        let (from, to) = (from.max(self.start), to.min(self.end));
        if from >= to {
            return Vec::new();
        }
        let pages: Vec<usize> = {
            #[cfg(feature = "diagnostics")]
            let waited = Timed(std::time::Instant::now(), crate::copystat::C::GuardWaitUs);
            let changed = self.changed.0.lock().unwrap_or_else(|e| e.into_inner());
            #[cfg(feature = "diagnostics")]
            drop(waited);
            changed.range(from / PAGE..to.div_ceil(PAGE)).copied().collect()
        };
        if pages.is_empty() {
            return Vec::new();
        }
        let whole: Vec<usize> = pages
            .iter()
            .copied()
            .filter(|&p| (p * PAGE).max(self.start) >= from && ((p + 1) * PAGE).min(self.end) <= to)
            .collect();
        // Every address showing the same pages is guarded too. A write in
        // between marks them changed again; the copy, made after, has it.
        #[cfg(feature = "diagnostics")]
        let waited = Timed(std::time::Instant::now(), crate::copystat::C::GuardWaitUs);
        let mut g = guards();
        let mut changed = self.changed.0.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(feature = "diagnostics")]
        drop(waited);
        #[cfg(feature = "diagnostics")]
        let asked = Timed(std::time::Instant::now(), crate::copystat::C::AliasUs);
        let others = aliases(&g, &whole);
        guard_aliases(&mut g, &others);
        #[cfg(feature = "diagnostics")]
        drop(asked);
        #[cfg(feature = "diagnostics")]
        let mut guarded = 0;
        for page in protect(&mut g, &whole) {
            changed.remove(&page);
            self.changed.taken(page);
            #[cfg(feature = "diagnostics")]
            {
                guarded += 1;
            }
        }
        self.changed.count(&changed);
        drop(changed);
        #[cfg(feature = "diagnostics")]
        {
            use crate::copystat::{C, add};
            add(C::TakenPages, pages.len() as u64);
            add(C::PartPages, (pages.len() - whole.len()) as u64);
            add(C::UnguardedPages, (whole.len() - guarded) as u64);
            g.copied.extend(&pages);
        }
        drop(g);
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for p in pages {
            let (a, b) = ((p * PAGE).max(from), ((p + 1) * PAGE).min(to));
            match runs.last_mut() {
                Some((_, end)) if *end == a => *end = b,
                _ => runs.push((a, b)),
            }
        }
        runs
    }

    /// Whether page `page` (a page number) changed since it was taken.
    pub(crate) fn changed(&self, page: usize) -> bool {
        self.changed
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&page)
    }

    /// Whether [`Tracked::take`] of `[from, to)` would return anything.
    pub(crate) fn any_changed(&self, from: usize, to: usize) -> bool {
        let (from, to) = (from.max(self.start), to.min(self.end));
        from < to
            && self.changed.2.load(Ordering::Acquire) != 0
            && self
                .changed
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .range(from / PAGE..to.div_ceil(PAGE))
                .next()
                .is_some()
    }

    /// The caller made pages (page numbers) of its copy equal to title
    /// memory itself (what shaders wrote went back): taken and guarded.
    pub(crate) fn taken(&self, pages: &[usize]) {
        let mut g = guards();
        let mut changed = self.changed.0.lock().unwrap_or_else(|e| e.into_inner());
        let others = aliases(&g, pages);
        guard_aliases(&mut g, &others);
        for page in protect(&mut g, pages) {
            changed.remove(&page);
            self.changed.taken(page);
        }
        self.changed.count(&changed);
    }

    /// Shaders may write the copy's bytes `[from, to)` until [`Tracked::disarm`]:
    /// pages there written next after they were taken have their bytes kept
    /// first ([`Tracked::saved`]).
    pub(crate) fn arm(&self, from: usize, to: usize) {
        let (from, to) = (from.max(self.start), to.min(self.end));
        if from < to {
            self.changed.1.lock().unwrap_or_else(|e| e.into_inner()).armed.extend(from / PAGE..to.div_ceil(PAGE));
        }
    }

    /// Page `page` (a page number) needs no saving any more: its copy-back
    /// is under way.
    pub(crate) fn unarm(&self, page: usize) {
        self.changed.1.lock().unwrap_or_else(|e| e.into_inner()).armed.remove(&page);
    }

    /// No shader writes the copy any more: nothing is armed or kept.
    pub(crate) fn disarm(&self) {
        let mut saved = self.changed.1.lock().unwrap_or_else(|e| e.into_inner());
        saved.armed.clear();
        saved.bytes.clear();
    }

    /// The bytes page `page` (a page number) held when it was first written
    /// since it was taken, if it was armed then (and so changed).
    pub(crate) fn saved(&self, page: usize) -> Option<Box<[u8]>> {
        self.changed.1.lock().unwrap_or_else(|e| e.into_inner()).bytes.remove(&page)
    }
}

/// The end of the allocation holding `at`, and whether it is private
/// memory (not a view), without `VirtualQuery`, which walks every page to
/// the end of a region: 10 us per MiB of a committed view, 60 ms for a
/// page of the runtime's 5.5 GiB pool view. `None` if not answered.
fn allocation(at: usize) -> Option<(usize, bool)> {
    use windows_sys::Win32::System::Memory::{
        MemoryRegionInfo, QueryVirtualMemoryInformation, WIN32_MEMORY_REGION_INFORMATION,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut info = WIN32_MEMORY_REGION_INFORMATION::default();
    // SAFETY: an out value of the right size for its class.
    let ok = unsafe {
        QueryVirtualMemoryInformation(
            GetCurrentProcess(),
            at as *const _,
            MemoryRegionInfo,
            (&raw mut info).cast(),
            size_of_val(&info),
            std::ptr::null_mut(),
        )
    } != 0;
    // SAFETY: plain flags.
    let private = unsafe { info.Anonymous.Flags } & 1 != 0;
    let end = (info.AllocationBase as usize).saturating_add(info.RegionSize);
    (ok && end > at).then_some((end, private))
}

/// Of pages `[first, last)`, the runs `[a, b)` holding plain committed
/// memory now (private and not write-watched: the C heap), which no guard
/// may cover. A page partly plain counts whole. Asked when guarding, not
/// when tracking: the heap commits pages of its reservations later, also
/// inside a loose copy's window. Title pools answer in one call, views in
/// one per view.
fn plain_pages(first: usize, last: usize) -> Vec<(usize, usize)> {
    use windows_sys::Win32::System::Memory::{
        GetWriteWatch, MEM_COMMIT, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    let watched = |at: usize, len: usize| {
        let (mut found, mut count, mut granularity) = (0usize, 1usize, 0u32);
        // SAFETY: room for one answer; no reset, so nothing changes.
        unsafe { GetWriteWatch(0, at as *const _, len, (&raw mut found).cast(), &mut count, &mut granularity) == 0 }
    };
    let (start, end) = (first * PAGE, last * PAGE);
    if watched(start, end - start) {
        return Vec::new();
    }
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut at = start;
    while at < end {
        // Views are never plain.
        if let Some((allocation_end, false)) = allocation(at) {
            at = allocation_end.min(end);
            continue;
        }
        #[cfg(test)]
        tests::REGION_WALKS.with(|n| n.set(n.get() + 1));
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        if unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) } == 0 {
            break;
        }
        let region_end = (info.BaseAddress as usize).saturating_add(info.RegionSize).min(end);
        if region_end <= at {
            break;
        }
        if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE && !watched(at, 4096) {
            let (a, b) = (at / PAGE, region_end.div_ceil(PAGE));
            match runs.last_mut() {
                Some((_, l)) if *l >= a => *l = (*l).max(b),
                _ => runs.push((a, b)),
            }
        }
        at = region_end;
    }
    runs
}

/// Makes `pages` read-only where not already (in runs), and returns those
/// now guarded or with nothing mapped (taken either way); a page that is
/// mapped but cannot be guarded, or holds plain memory ([`plain_pages`]),
/// is left out (it stays changed).
fn protect(g: &mut Guards, pages: &[usize]) -> Vec<usize> {
    #[cfg(feature = "diagnostics")]
    let counted = |calls: u64, pages: u64| {
        crate::copystat::add(crate::copystat::C::ProtectCalls, calls);
        crate::copystat::add(crate::copystat::C::ProtectPages, pages);
    };
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READONLY, VirtualProtect, VirtualQuery,
    };
    #[cfg(feature = "diagnostics")]
    let _timed = Timed(std::time::Instant::now(), crate::copystat::C::ProtectUs);
    let set = |from: usize, to: usize| {
        let mut old = 0;
        // SAFETY: title memory; only its protection changes.
        unsafe { VirtualProtect((from * PAGE) as *const _, (to - from) * PAGE, PAGE_READONLY, &mut old) != 0 }
    };
    let unmapped = |page: usize| {
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        let n = unsafe { VirtualQuery((page * PAGE) as *const _, &mut info, size_of_val(&info)) };
        n == 0 || info.State != MEM_COMMIT
    };
    let mut taken = Vec::with_capacity(pages.len());
    let busy = |page: usize| g.busy.iter().any(|&(a, b)| (a..b).contains(&page));
    let pages: Vec<usize> = pages.iter().copied().filter(|&p| !busy(p)).collect();
    let mut i = 0;
    while i < pages.len() {
        if g.protected.contains(&pages[i]) {
            taken.push(pages[i]);
            i += 1;
            continue;
        }
        // A run of consecutive pages not guarded yet.
        let first = i;
        while i + 1 < pages.len() && pages[i + 1] == pages[i] + 1 && !g.protected.contains(&pages[i + 1]) {
            i += 1;
        }
        let run = &pages[first..=i];
        i += 1;
        #[cfg(feature = "diagnostics")]
        counted(1, run.len() as u64);
        let plain = plain_pages(run[0], run[run.len() - 1] + 1);
        if plain.is_empty() && set(run[0], run[run.len() - 1] + 1) {
            g.protected.extend(run);
            taken.extend(run);
            continue;
        }
        for &page in run {
            if plain.iter().any(|&(a, b)| (a..b).contains(&page)) {
                // Left out: it stays changed, copied whole each time.
                continue;
            }
            if set(page, page + 1) {
                g.protected.insert(page);
                taken.push(page);
            } else if unmapped(page) {
                taken.push(page);
            }
        }
    }
    taken
}

/// Pages `[first, last)` changed, and so the pages showing the same memory
/// elsewhere: gives write access back where we guarded them and marks them
/// in every tracker holding them. True if one of `[first, last)` was
/// guarded.
fn changed(g: &mut Guards, first: usize, last: usize, save: bool) -> bool {
    changed_in(g, first, last, save, true)
}

/// [`changed`]; with `open_guarded` false, guarded pages stay guarded: a
/// write that has landed already (GPU work in place) needs no access, and
/// the title's next write faults and marks them again, so the guard and
/// the tracker's next take need no protection change.
fn changed_in(g: &mut Guards, first: usize, last: usize, save: bool, open_guarded: bool) -> bool {
    let guarded = changed_here(g, first, last, save, open_guarded);
    let others: Vec<usize> = g.alias_of.range(first..last).flat_map(|(_, o)| o.iter().copied()).collect();
    #[cfg(feature = "diagnostics")]
    {
        let held = others.iter().filter(|p| g.holders.contains_key(p)).count();
        crate::copystat::add(crate::copystat::C::AliasMarks, held as u64);
    }
    for page in others {
        changed_here(g, page, page + 1, save, open_guarded);
    }
    guarded
}

/// [`changed_in`] of `[first, last)` alone. With `save`, before the write
/// lands: trackers armed for a page they had taken keep its bytes.
fn changed_here(g: &mut Guards, first: usize, last: usize, save: bool, open_guarded: bool) -> bool {
    let guarded: Vec<usize> = g.protected.range(first..last).copied().collect();
    if open_guarded {
        for &page in &guarded {
            open(g, page);
        }
    }
    for (&page, holders) in g.holders.range(first..last) {
        for c in holders {
            let mut set = c.0.lock().unwrap_or_else(|e| e.into_inner());
            let first_write = set.insert(page);
            c.count(&set);
            drop(set);
            if first_write && save {
                c.save(page);
            }
        }
        #[cfg(feature = "diagnostics")]
        g.written.insert(page);
    }
    !guarded.is_empty()
}

/// At each presented frame (diagnostics): counts the tracked pages written
/// in it, and those written the frame before too.
#[cfg(feature = "diagnostics")]
pub(crate) fn frame_done() {
    let mut g = guards();
    let written = std::mem::take(&mut g.written);
    let again = written.intersection(&g.written_before).count();
    crate::copystat::add(crate::copystat::C::WrittenPages, written.len() as u64);
    crate::copystat::add(crate::copystat::C::WrittenAgain, again as u64);
    crate::memclass::frame(g.holders.keys().map(|p| p * PAGE), written.iter().map(|p| p * PAGE));
    g.written_before = written;
    let copied = std::mem::take(&mut g.copied);
    crate::copystat::add(crate::copystat::C::DistinctTakenPages, copied.len() as u64);
}

/// A write into title memory the guards do not see, under way: its pages
/// stay open (a tracker taking them meanwhile would guard them, and the
/// kernel's write would fail) until it is dropped, which marks them changed
/// once more.
#[must_use = "the write must happen while this lives"]
pub struct Writing {
    first: usize,
    last: usize,
    /// Diagnostics: the bytes written, scanned once written for the empty
    /// bounds the title faulted on, and what to call a hit in the trail.
    #[cfg(feature = "diagnostics")]
    scan: (usize, usize, &'static str),
}

impl Drop for Writing {
    fn drop(&mut self) {
        {
            let mut g = guards();
            if let Some(i) = g.busy.iter().position(|&r| r == (self.first, self.last)) {
                g.busy.swap_remove(i);
            }
            changed(&mut g, self.first, self.last, false);
        }
        #[cfg(feature = "diagnostics")]
        crate::trail::scan_empty_bounds(self.scan.0, self.scan.1, self.scan.2);
    }
}

/// Before our own write into title memory `[at, at + len)` through the
/// kernel (which does not fault into our handler) or another write the
/// guards do not see: opens its guarded blocks, marks them changed and
/// keeps them open while the result lives. The runtime calls it (as
/// `kernel_will_write`) around the title's kernel reads into its own
/// memory and DirectStorage's writes to addresses.
pub fn kernel_will_write(at: usize, len: usize) -> Writing {
    #[cfg(feature = "diagnostics")]
    crate::memclass::wrote(crate::memclass::Writer::Kernel, at, len);
    #[cfg(feature = "diagnostics")]
    crate::trail::step_if_seen(at / PAGE, at.saturating_add(len).div_ceil(PAGE), "written by the runtime or the kernel");
    #[cfg(feature = "diagnostics")]
    crate::trail::wrote(at, len, "written by the runtime or the kernel", None);
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_mut))]
    let mut w = writing(at, len);
    #[cfg(feature = "diagnostics")]
    {
        w.scan = (at, len, "the runtime or the kernel wrote -1 beside infinities (empty bounds)");
    }
    w
}

/// [`writing`] for what GPU work wrote: write-backs of host copies, and
/// copies the title's lists make into title memory.
pub(crate) fn gpu_writing(at: usize, len: usize) -> Writing {
    #[cfg(feature = "diagnostics")]
    crate::memclass::wrote(crate::memclass::Writer::Gpu, at, len);
    #[cfg(feature = "diagnostics")]
    crate::trail::step_if_seen(at / PAGE, at.saturating_add(len).div_ceil(PAGE), "written by GPU work");
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_mut))]
    let mut w = writing(at, len);
    #[cfg(feature = "diagnostics")]
    {
        w.scan = (at, len, "GPU work wrote -1 beside infinities (empty bounds)");
    }
    w
}

/// After GPU work wrote `[at, at + len)` where it lies (copies used in
/// place, `crate::inplace`): trackers of those pages and of the other
/// addresses showing them (`alias_of`) see a write, as [`gpu_writing`]
/// shows them a copy-back's. Without it a host copy or a texture over
/// another mapping of the pages keeps the bytes from before the GPU's.
pub(crate) fn gpu_wrote(at: usize, len: usize) {
    if len == 0 {
        return;
    }
    let (first, last) = (at / PAGE, at.saturating_add(len).div_ceil(PAGE));
    #[cfg(feature = "diagnostics")]
    {
        crate::memclass::wrote(crate::memclass::Writer::Gpu, at, len);
        crate::trail::step_if_seen(first, last, "written by GPU work in place");
        crate::trail::wrote(at, len, "written by GPU work in place", None);
    }
    // The ranges are what lists declare (up to a whole copy), most of it
    // not written: opening their guarded pages cost ~6000 protection
    // changes a world frame (rb-h12-1).
    changed_in(&mut guards(), first, last, false, false);
    // Scanned as far as a root view declares (1 MiB): bigger ranges are
    // whole buffers the shaders write a part of.
    #[cfg(feature = "diagnostics")]
    crate::trail::scan_empty_bounds(at, len.min(1 << 20), "GPU work in place wrote -1 beside infinities (empty bounds)");
}

/// Diagnostics: the other addresses (page starts) the title shows the page
/// holding `at` at (`alias_of`).
#[cfg(feature = "diagnostics")]
pub(crate) fn shown_elsewhere(at: usize) -> Vec<usize> {
    guards().alias_of.get(&(at / PAGE)).map_or_else(Vec::new, |o| o.iter().map(|p| p * PAGE).collect())
}

/// What [`kernel_will_write`] and [`gpu_writing`] do.
fn writing(at: usize, len: usize) -> Writing {
    // The length is the title's (`fread`'s size times count, saturated).
    let (first, last) = (at / PAGE, at.saturating_add(len).div_ceil(PAGE));
    let mut g = guards();
    g.busy.push((first, last));
    changed(&mut g, first, last, true);
    Writing {
        first,
        last,
        #[cfg(feature = "diagnostics")]
        scan: (at, 0, ""),
    }
}

/// The runtime mapped or unmapped title memory `[at, at + len)`: what it
/// shows is new, and read-write. Called before an unmap.
pub fn title_memory_remapped(at: usize, len: usize) {
    // Copies the GPU uses in place go first, before the runtime unmaps:
    // those opened by address pin the pages, and the addresses will show
    // other memory. Their section views are forgotten with them.
    crate::shadow::let_go(at as u64, len as u64);
    let (first, last) = (at / PAGE, at.saturating_add(len).div_ceil(PAGE));
    #[cfg(feature = "diagnostics")]
    crate::trail::step_if_seen(first, last, "mapped or unmapped");
    #[cfg(feature = "diagnostics")]
    crate::trail::wrote(at, len, "mapped or unmapped", None);
    let mut g = guards();
    // A page a disk load left without bytes that another address still
    // shows, left so by the same load, keeps its bytes there; the others
    // are filled before they go (`open`, below): the physical page may be
    // mapped again later.
    let lazy: Vec<usize> = g.lazy.range(first..last).map(|(&p, _)| p).collect();
    for page in lazy {
        let (load, offset) = g.lazy[&page].clone();
        let shown = g.alias_of.get(&page).is_some_and(|others| {
            others.iter().any(|o| {
                !(first..last).contains(o)
                    && g.lazy.get(o).is_some_and(|(l, a)| Arc::ptr_eq(l, &load) && *a == offset)
            })
        });
        if shown {
            #[cfg(feature = "diagnostics")]
            crate::trail::step(page, "unmapped without a fill: shown at another address", Some((&load, offset)));
            g.lazy.remove(&page);
            g.protected.remove(&page);
            crate::ledger::add(crate::ledger::Owner::LazyPages, -(PAGE as i64));
        }
    }
    // What it showed is gone already: nothing to keep.
    changed(&mut g, first, last, false);
    // What it showed is not what it shows now: its links go, both ways.
    let stale: Vec<usize> = g.alias_of.range(first..last).map(|(&p, _)| p).collect();
    for page in stale {
        for other in g.alias_of.remove(&page).unwrap_or_default() {
            if let Some(links) = g.alias_of.get_mut(&other) {
                links.retain(|&p| p != page);
                if links.is_empty() {
                    g.alias_of.remove(&other);
                }
            }
        }
    }
}

/// The runtime mapped the page at `at` where `others` show it already: the
/// pages are linked both ways, and if we guard it there, it is guarded here
/// too (a write through the new mapping must not go unseen by the trackers
/// there).
pub fn title_memory_aliased(at: usize, others: &[usize]) {
    let mut g = guards();
    let page = at / PAGE;
    for &other in others {
        for (x, y) in [(other / PAGE, page), (page, other / PAGE)] {
            let links = g.alias_of.entry(x).or_default();
            if !links.contains(&y) {
                links.push(y);
            }
        }
    }
    // Shows a page a disk load left without bytes: left so here too.
    let lazy = others.iter().find_map(|&o| g.lazy.get(&(o / PAGE)).cloned());
    if let Some(entry) = lazy {
        use windows_sys::Win32::System::Memory::PAGE_NOACCESS;
        #[cfg(feature = "diagnostics")]
        crate::trail::step(page, "mapped where a load left the page without bytes", Some((&entry.0, entry.1)));
        if set_protection(page, PAGE_NOACCESS) {
            g.protected.insert(page);
            if g.lazy.insert(page, entry).is_none() {
                crate::ledger::add(crate::ledger::Owner::LazyPages, PAGE as i64);
            }
            return;
        }
        #[cfg(feature = "diagnostics")]
        crate::trail::step(page, "its protection failed: readable without bytes", None);
    }
    if others.iter().any(|&o| g.protected.contains(&(o / PAGE))) {
        protect(&mut g, &[page]);
    }
}

/// Registers the handler that takes writes to guarded blocks, once, ahead
/// of the crash reporter's.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_write(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        const WRITE: usize = 1;
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS || r.NumberParameters < 2 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Reads and other faults are ours only once loads have left pages
        // without bytes: the lock is not taken for others (a fault in code
        // holding it would wait forever).
        let lazy_seen = crate::ledger::bytes(crate::ledger::Owner::LazyPages) > 0
            || crate::ledger::bytes(crate::ledger::Owner::LazyFilled) > 0;
        if r.ExceptionInformation[0] != WRITE && !lazy_seen {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let at = r.ExceptionInformation[1];
        #[cfg(feature = "diagnostics")]
        let _timed = Timed(std::time::Instant::now(), crate::copystat::C::HandlerUs);
        // Any access to a page a disk load left without bytes fills it; a
        // write then faults again on the guard as any other.
        if lazy_seen && fill_unlocked(at / PAGE) {
            crate::ledger::add(crate::ledger::Owner::LazyFaults, PAGE as i64);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
        if r.ExceptionInformation[0] != WRITE {
            // A read of a page another thread filled since this one
            // faulted: it reads again.
            const READ: usize = 0;
            return if r.ExceptionInformation[0] == READ && readable(at) {
                EXCEPTION_CONTINUE_EXECUTION
            } else {
                EXCEPTION_CONTINUE_SEARCH
            };
        }
        // Held across all checks: another thread may have opened the block
        // since this one faulted, and a copy may have guarded it again since
        // (then it is ours again, not read-only for some other reason).
        let mut g = guards();
        if changed(&mut g, at / PAGE, at / PAGE + 1, true) {
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::GuardFaults, 1);
            #[cfg(feature = "diagnostics")]
            crate::memclass::wrote(crate::memclass::Writer::Cpu, at, 1);
            #[cfg(feature = "diagnostics")]
            crate::trail::step_if_seen(at / PAGE, at / PAGE + 1, "written by the title");
            #[cfg(feature = "diagnostics")]
            crate::trail::wrote(at, 1, "written by the title", None);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
        if writable(at) {
            EXCEPTION_CONTINUE_EXECUTION
        } else {
            EXCEPTION_CONTINUE_SEARCH
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler only touches our guard state and protections.
    ONCE.call_once(|| unsafe {
        AddVectoredExceptionHandler(1, Some(on_write));
    });
}

/// Whether `at` is committed memory a read may touch now.
fn readable(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0 && info.Protect != 0
}

/// Whether `at` is committed read-write memory now.
fn writable(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE
}

/// Adds the time since it was made to a counter when dropped (diagnostics).
#[cfg(feature = "diagnostics")]
pub(crate) struct Timed(pub(crate) std::time::Instant, pub(crate) crate::copystat::C);

#[cfg(feature = "diagnostics")]
impl Drop for Timed {
    fn drop(&mut self) {
        let c = std::mem::replace(&mut self.1, crate::copystat::C::Submissions);
        crate::copystat::add(c, self.0.elapsed().as_micros() as u64);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };
    pub(crate) const MEM_WRITE_WATCH: u32 = 0x20_0000;

    thread_local! {
        /// `plain_pages`' region queries on this thread.
        pub(super) static REGION_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// A page of a big committed view (the runtime's pool view of its
    /// physical pages) is not plain, found without a region query, which
    /// walks the view to its end (60 ms a page in the title's menus).
    #[test]
    fn plain_pages_skip_views_without_walking_them() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            SEC_RESERVE, UnmapViewOfFile,
        };
        const SIZE: usize = 256 << 20;
        // SAFETY: a pagefile-backed section, one view of it, its pages
        // committed (charged, never touched); all released below.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE | SEC_RESERVE,
                0,
                SIZE as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, SIZE).Value as usize;
            assert_ne!(view, 0);
            assert!(!VirtualAlloc(view as *const _, SIZE, MEM_COMMIT, PAGE_READWRITE).is_null());
            (section, view)
        };
        let page = view.div_ceil(PAGE) + 1;
        REGION_WALKS.with(|n| n.set(0));
        assert_eq!(plain_pages(page, page + 2), [], "a view is never plain");
        assert_eq!(REGION_WALKS.with(|n| n.get()), 0, "nor walked");
        // SAFETY: the view and section made above.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut _ });
            CloseHandle(section);
        }
    }

    /// A write-watched allocation of `pages` pages (as the runtime's pools),
    /// on a 64 KiB boundary (the allocation granularity).
    pub(crate) fn pool(pages: usize) -> usize {
        // SAFETY: a fresh allocation, freed by the test or leaked.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                pages * PAGE,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        } as usize;
        assert!(p != 0 && p.is_multiple_of(PAGE));
        p
    }

    /// `words` dwords counting up from 0 in memory that owns the whole 4 MiB
    /// window around it ([`crate::shadow`]'s loose copies take windows):
    /// never on the host heap. A copy of a window write-protects all of it,
    /// so one over the heap faults the host's own allocations into the
    /// handler on the thread holding the guards, which then waits for itself
    /// (a hang about every second run). Leaked: the loose copy outlives the
    /// test (shortcut: never freed).
    pub(crate) fn owned_window(words: usize) -> *const u32 {
        const WINDOW: usize = 4 << 20;
        assert!(words * 4 <= WINDOW);
        // SAFETY: a fresh allocation, leaked.
        let p = unsafe {
            VirtualAlloc(std::ptr::null(), 2 * WINDOW, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)
        } as usize;
        assert!(p != 0);
        let start = p.next_multiple_of(WINDOW) as *mut u32;
        for k in 0..words {
            // SAFETY: inside the allocation.
            unsafe { start.add(k).write(k as u32) };
        }
        start
    }

    pub(crate) fn free(p: usize) {
        // SAFETY: an allocation of ours.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
    }

    /// Everything is changed at first; after taking, only what is written
    /// (by faults, or reported) is, and every tracker of a page sees it.
    #[test]
    fn writes_mark_every_tracker_of_their_page() {
        let p = pool(4);
        let a = track(p, 4 * PAGE, true).expect("pool memory is tracked");
        let b = track(p + PAGE, 2 * PAGE, true).expect("tracked");
        assert_eq!(a.take(p, p + 4 * PAGE), [(p, p + 4 * PAGE)]);
        assert_eq!(b.take(p, p + 4 * PAGE), [(p + PAGE, p + 3 * PAGE)]);
        assert!(a.take(p, p + 4 * PAGE).is_empty() && b.take(p, p + 4 * PAGE).is_empty());
        // SAFETY: committed pages of ours, now read-only: the handler opens
        // the block; the second write needs no fault.
        unsafe {
            ((p + PAGE + 8) as *mut u8).write_volatile(1);
            ((p + PAGE + 9) as *mut u8).write_volatile(2);
        }
        assert_eq!(a.take(p, p + 4 * PAGE), [(p + PAGE, p + 2 * PAGE)]);
        // `a` guarded it again; `b` still has it changed.
        assert!(b.changed(p / PAGE + 1));
        assert_eq!(b.take(p, p + 4 * PAGE), [(p + PAGE, p + 2 * PAGE)]);
        // SAFETY: as above: a fault again, for both.
        unsafe { ((p + PAGE + 10) as *mut u8).write_volatile(3) };
        assert!(a.changed(p / PAGE + 1) && b.changed(p / PAGE + 1));
        assert_eq!(a.take(p, p + 4 * PAGE), [(p + PAGE, p + 2 * PAGE)]);
        drop(writing(p + 3 * PAGE + 5, 1));
        assert_eq!(a.take(p, p + 4 * PAGE), [(p + 3 * PAGE, p + 4 * PAGE)]);
        title_memory_remapped(p, PAGE);
        assert!(a.changed(p / PAGE) && !a.changed(p / PAGE + 1));
        drop((a, b));
        free(p);
    }

    /// Pages a disk load leaves without bytes: changed for trackers, read
    /// from the load by our reads without being filled, filled by a title
    /// access (then guarded as before), and filled before our writes.
    #[test]
    fn pages_left_without_bytes_fill_on_access() {
        struct Pattern;
        impl crate::lazy::Source for Pattern {
            fn size(&self) -> usize {
                2 * PAGE
            }
            fn whole(&self) -> bool {
                false
            }
            fn read(&self, at: usize, out: &mut [u8]) -> bool {
                for (i, b) in out.iter_mut().enumerate() {
                    *b = ((at + i) % 251) as u8;
                }
                true
            }
        }
        let byte = |at: usize| (at % 251) as u8;
        let p = pool(3);
        let t = track(p, 3 * PAGE, true).expect("pool memory is tracked");
        assert_eq!(t.take(p, p + 3 * PAGE), [(p, p + 3 * PAGE)]);
        let load = crate::lazy::Load::new(Box::new(Pattern));
        assert!(title_memory_lazy(&load, p + PAGE, 0));
        assert!(title_memory_lazy(&load, p + 2 * PAGE, PAGE));
        assert!(!title_memory_lazy(&load, p + 5, 0), "not a page start");
        assert_eq!(t.take(p, p + 3 * PAGE), [(p + PAGE, p + 3 * PAGE)], "loaded pages changed");
        // Our read: page 0 from memory, the others from the load.
        let mut out = vec![9u8; 3 * PAGE - 10];
        assert!(crate::host::read_into((p + 10) as *const u8, &mut out));
        assert_eq!(out[0], 0);
        assert_eq!(out[PAGE - 10 + 3], byte(3));
        assert_eq!(out[2 * PAGE - 10 + 7], byte(PAGE + 7));
        assert!(guards().lazy.contains_key(&(p / PAGE + 1)), "a read does not fill");
        // SAFETY: a committed page of ours without access: the handler
        // fills it.
        let v = unsafe { ((p + 2 * PAGE + 7) as *const u8).read_volatile() };
        assert_eq!(v, byte(PAGE + 7));
        assert!(!guards().lazy.contains_key(&(p / PAGE + 2)));
        // Tracked, so guarded again: a write is seen.
        // SAFETY: as above, now read-only.
        unsafe { ((p + 2 * PAGE + 7) as *mut u8).write_volatile(1) };
        assert_eq!(t.take(p, p + 3 * PAGE), [(p + 2 * PAGE, p + 3 * PAGE)]);
        // Our write fills the page first.
        drop(writing(p + PAGE + 1, 1));
        // SAFETY: filled and open now.
        assert_eq!(unsafe { ((p + PAGE + 2) as *const u8).read_volatile() }, byte(2));
        assert!(guards().lazy.is_empty() || !guards().lazy.range(p / PAGE..p / PAGE + 3).any(|_| true));
        // A reservation released with pages left so drops them unfilled.
        assert!(title_memory_lazy(&load, p + PAGE, 0));
        title_memory_freed(p);
        assert!(!guards().lazy.range(p / PAGE..p / PAGE + 3).any(|_| true));
        drop(t);
        free(p);
    }

    /// A page our write fills from its load stays without access while the
    /// load's bytes are read (a disk read or a decode, milliseconds): open
    /// before, the title's threads would read it unfilled, without faulting.
    #[test]
    fn a_fill_reads_its_bytes_before_the_page_opens() {
        use std::sync::atomic::Ordering::Relaxed;
        use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, PAGE_NOACCESS, VirtualQuery};
        struct Checking(usize, Arc<std::sync::atomic::AtomicU32>);
        impl crate::lazy::Source for Checking {
            fn size(&self) -> usize {
                PAGE
            }
            fn whole(&self) -> bool {
                false
            }
            fn read(&self, _: usize, out: &mut [u8]) -> bool {
                // SAFETY: plain data filled in by the query.
                let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
                // SAFETY: a valid out pointer of the right size.
                unsafe { VirtualQuery(self.0 as *const _, &mut info, size_of_val(&info)) };
                self.1.store(info.Protect, Relaxed);
                out.fill(7);
                true
            }
        }
        let p = pool(2);
        let seen = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let load = crate::lazy::Load::new(Box::new(Checking(p + PAGE, seen.clone())));
        assert!(title_memory_lazy(&load, p + PAGE, 0));
        drop(writing(p + PAGE + 1, 1));
        assert_eq!(seen.load(Relaxed), PAGE_NOACCESS, "the page opened before its bytes were read");
        // SAFETY: filled and open now.
        assert_eq!(unsafe { ((p + PAGE + 2) as *const u8).read_volatile() }, 7);
        free(p);
    }

    /// A page only partly in the range asked is copied in part and stays
    /// changed; edge pages count whole when the tracked part is covered.
    #[test]
    fn partial_pages_stay_changed() {
        let p = pool(3);
        let t = track(p + 100, 2 * PAGE, true).expect("tracked");
        assert_eq!(t.take(p + 100, p + PAGE + 10), [(p + 100, p + PAGE + 10)]);
        assert!(!t.changed(p / PAGE) && t.changed(p / PAGE + 1));
        assert_eq!(t.take(p, p + 3 * PAGE), [(p + PAGE, p + 2 * PAGE + 100)]);
        assert!(t.take(p, p + 3 * PAGE).is_empty());
        drop(t);
        free(p);
    }

    /// [`Tracked::any_changed`] says whether a take would return anything.
    #[test]
    fn any_changed_agrees_with_take() {
        let p = pool(4);
        let t = track(p + 100, 3 * PAGE, true).expect("tracked");
        let (start, end) = (p + 100, p + 100 + 3 * PAGE);
        assert!(t.any_changed(start, end), "all changed at first");
        t.take(start, end);
        assert!(!t.any_changed(start, end));
        // SAFETY: our pool, guarded since the take.
        unsafe { ((p + 2 * PAGE + 8) as *mut u8).write_volatile(1) };
        assert!(t.any_changed(start, end) && t.any_changed(p + 2 * PAGE, p + 2 * PAGE + 1));
        assert!(!t.any_changed(start, p + 2 * PAGE), "the pages before it");
        assert!(!t.any_changed(p + 3 * PAGE, end), "the pages after it");
        // Outside the tracked memory, or empty.
        assert!(!t.any_changed(p, start) && !t.any_changed(end, end + PAGE));
        assert!(!t.any_changed(p + 2 * PAGE, p + 2 * PAGE));
        // A page taken in part stays changed.
        assert_eq!(t.take(p + 2 * PAGE, p + 2 * PAGE + 16).len(), 1);
        assert!(t.any_changed(start, end));
        assert_eq!(t.take(start, end).len(), 1);
        assert!(!t.any_changed(start, end));
        drop(t);
        free(p);
    }

    /// Every way a page is marked changed reaches the check, also on a
    /// tracker with no changed page left (where the check skips its lock):
    /// a kernel write, a remap, and a page taken by copy-back then written.
    #[test]
    fn a_quiet_tracker_sees_every_kind_of_write() {
        let p = pool(4);
        let t = track(p, 4 * PAGE, true).expect("tracked");
        let all = (p, p + 4 * PAGE);
        let quiet = |t: &Tracked| {
            t.take(all.0, all.1);
            assert!(!t.any_changed(all.0, all.1), "nothing changed after a take");
        };
        quiet(&t);
        drop(kernel_will_write(p + PAGE + 3, 10));
        assert!(t.any_changed(p + PAGE, p + 2 * PAGE), "a kernel write");
        quiet(&t);
        title_memory_remapped(p + 2 * PAGE, PAGE);
        assert!(t.any_changed(p + 2 * PAGE, p + 3 * PAGE), "a remap");
        quiet(&t);
        // Copy-back made page 3 equal to title memory: taken, then written.
        let page = (p + 3 * PAGE) / PAGE;
        drop(gpu_writing(p + 3 * PAGE, 8));
        t.taken(&[page]);
        assert!(!t.any_changed(all.0, all.1), "taken by copy-back");
        // SAFETY: our pool, guarded since it was taken.
        unsafe { ((p + 3 * PAGE + 8) as *mut u8).write_volatile(1) };
        assert!(t.any_changed(p + 3 * PAGE, p + 4 * PAGE), "a title write after copy-back");
        drop(t);
        free(p);
    }

    /// Plain committed memory (the C heap) is not tracked; reserved memory
    /// is, reads as taken, and changes when the runtime maps it.
    #[test]
    fn heap_memory_is_not_tracked_and_placeholders_wait_for_mappings() {
        let plain = vec![0u8; 2 * PAGE];
        assert!(track(plain.as_ptr() as usize, plain.len(), true).is_none());
        // SAFETY: a reservation of ours, freed below.
        let r = unsafe { VirtualAlloc(std::ptr::null(), 2 * PAGE, MEM_RESERVE, PAGE_READWRITE) } as usize;
        assert_ne!(r, 0);
        let t = track(r, 2 * PAGE, true).expect("reserved memory is tracked");
        assert_eq!(t.take(r, r + 2 * PAGE).len(), 1, "copied once (as zeros)");
        assert!(t.take(r, r + 2 * PAGE).is_empty());
        title_memory_remapped(r, PAGE);
        assert_eq!(t.take(r, r + 2 * PAGE), [(r, r + PAGE)]);
        drop(t);
        // SAFETY: our reservation.
        unsafe { VirtualFree(r as *mut _, 0, MEM_RELEASE) };
    }

    fn protection(at: usize) -> u32 {
        use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        assert_ne!(unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) }, 0);
        info.Protect
    }

    /// Plain committed memory in a tracked range (a loose copy's window
    /// over the C heap, which commits its reserved pages later) is never
    /// guarded: copied every time, writable without our handler.
    #[test]
    fn plain_memory_in_a_tracked_range_is_never_guarded() {
        use windows_sys::Win32::System::Memory::PAGE_READONLY;
        // SAFETY: a reservation of ours (tracked, as a placeholder); its
        // second page committed as plain private memory once tracked.
        let r = unsafe { VirtualAlloc(std::ptr::null(), 2 * PAGE, MEM_RESERVE, PAGE_READWRITE) } as usize;
        assert_ne!(r, 0);
        let t = track(r, 2 * PAGE, true).expect("a reservation is tracked");
        // SAFETY: inside our reservation.
        let plain = unsafe { VirtualAlloc((r + PAGE) as *const _, PAGE, MEM_COMMIT, PAGE_READWRITE) } as usize;
        assert_eq!(plain, r + PAGE);
        assert_eq!(t.take(r, r + 2 * PAGE), [(r, r + 2 * PAGE)], "all of it at first");
        assert_eq!(t.take(r, r + 2 * PAGE), [(r + PAGE, r + 2 * PAGE)], "the plain page every time");
        assert_ne!(protection(plain), PAGE_READONLY, "not guarded by a take");
        t.taken(&[r / PAGE + 1]);
        assert_ne!(protection(plain), PAGE_READONLY, "not guarded by a copy-back");
        assert!(t.changed(r / PAGE + 1));
        drop(t);
        // SAFETY: our reservation.
        unsafe { VirtualFree(r as *mut _, 0, MEM_RELEASE) };
    }

    /// A run of pages from a write-watched pool into plain memory next to
    /// it: the pool's page is guarded, the plain one is not.
    #[test]
    fn a_run_from_a_pool_into_plain_memory_guards_only_the_pool() {
        use windows_sys::Win32::System::Memory::PAGE_READONLY;
        // Two pages of address space, then a pool and plain memory in them.
        let r = pool(2);
        free(r);
        // SAFETY: fresh allocations where we just freed ours (another
        // thread may take the space first: then there is nothing to test).
        let (p, plain) = unsafe {
            let p = VirtualAlloc(r as *const _, PAGE, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) as usize;
            let plain = VirtualAlloc((r + PAGE) as *const _, PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) as usize;
            (p, plain)
        };
        if p == r && plain == r + PAGE {
            let t = track(r, 2 * PAGE, true).expect("pool memory is tracked");
            assert_eq!(t.take(r, r + 2 * PAGE), [(r, r + 2 * PAGE)]);
            assert_eq!(protection(r), PAGE_READONLY, "the pool's page guarded");
            assert_ne!(protection(plain), PAGE_READONLY, "the plain page not");
            assert_eq!(t.take(r, r + 2 * PAGE), [(r + PAGE, r + 2 * PAGE)]);
            drop(t);
        }
        for a in [p, plain].into_iter().filter(|&a| a != 0) {
            free(a);
        }
    }

    /// A kernel read into a guarded view block fails; opened first, it
    /// lands and the block counts as changed.
    #[test]
    fn kernel_reads_into_guarded_blocks_after_opening() {
        use std::io::{Read, Seek, Write};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of two blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * PAGE) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * PAGE).Value as usize
        };
        assert_ne!(view, 0);
        assert!(track(view, 2 * PAGE, false).is_none(), "views only where asked");
        let t = track(view, 2 * PAGE, true).expect("views are tracked");
        assert_eq!(t.take(view, view + 2 * PAGE), [(view, view + 2 * PAGE)]); // now read-only
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"from the disk").unwrap();
        // `ReadFile` inside: the kernel writes the view.
        let read = |file: &mut std::fs::File| {
            file.rewind().unwrap();
            // SAFETY: 13 bytes of our view, nothing else refers to them.
            let into = unsafe { std::slice::from_raw_parts_mut((view + 100) as *mut u8, 13) };
            file.read(into).is_ok_and(|n| n == 13)
        };
        assert!(!read(&mut file), "the kernel does not fault into our handler");
        let write = writing(view + 100, 13);
        // A tracker taking the page meanwhile leaves it open.
        assert_eq!(t.take(view, view + 2 * PAGE), [(view, view + PAGE)]);
        assert!(read(&mut file));
        drop(write);
        // SAFETY: the bytes just read.
        assert_eq!(unsafe { std::slice::from_raw_parts((view + 100) as *const u8, 13) }, b"from the disk");
        assert_eq!(t.take(view, view + 2 * PAGE), [(view, view + PAGE)]);
        drop(t);
        // SAFETY: our view.
        unsafe { UnmapViewOfFile(windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut _ }) };
    }

    /// `pages` pages of a fresh pagefile section mapped twice (as the
    /// title's pool view and tiled heap show the same physical pages),
    /// reported as the runtime does when it maps them.
    pub(crate) fn twin_views(pages: usize) -> (usize, usize) {
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile};
        let len = pages * PAGE;
        // SAFETY: a pagefile section mapped read-write twice; the test
        // keeps both views.
        let (a, b) = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                len as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (
                MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, len).Value as usize,
                MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, len).Value as usize,
            )
        };
        assert!(a != 0 && b != 0);
        for k in (0..len).step_by(PAGE) {
            title_memory_aliased(b + k, &[a + k]);
        }
        (a, b)
    }

    /// A write through another mapping of the same pages marks the tracker
    /// (the guard there faults, the runtime says where else it is mapped);
    /// so does a write the runtime reports at the other mapping.
    #[test]
    fn writes_through_another_mapping_mark_the_tracker() {
        let (a, b) = twin_views(2);
        let t = track(a, 2 * PAGE, true).expect("views are tracked");
        assert_eq!(t.take(a, a + 2 * PAGE), [(a, a + 2 * PAGE)]);
        // SAFETY: the second view of the pages, guarded by the take.
        unsafe { ((b + PAGE + 4) as *mut u32).write_volatile(7) };
        assert_eq!(t.take(a, a + 2 * PAGE), [(a + PAGE, a + 2 * PAGE)]);
        // SAFETY: the tracked view, which shows the write.
        assert_eq!(unsafe { ((a + PAGE + 4) as *const u32).read_volatile() }, 7);
        drop(writing(b, 8));
        assert_eq!(t.take(a, a + 2 * PAGE), [(a, a + PAGE)]);
        assert!(t.take(a, a + 2 * PAGE).is_empty());
        // A third mapping made later starts guarded, so its writes count.
        let (c, _) = twin_views(1);
        title_memory_aliased(c, &[a]);
        // SAFETY: a view of ours (another section: only the tracking
        // takes it for `a`'s pages here).
        unsafe { (c as *mut u32).write_volatile(1) };
        assert_eq!(t.take(a, a + 2 * PAGE), [(a, a + PAGE)]);
    }

    /// A write or mapping that runs past the end of the address space (a
    /// title's `fread` with a huge count) covers up to the end: the sum must
    /// not overflow. Up there no test has guarded blocks.
    #[test]
    fn ranges_past_the_address_space_end_do_not_overflow() {
        let top = usize::MAX - 16;
        drop(writing(top, PAGE));
        title_memory_remapped(top, usize::MAX);
    }
}
