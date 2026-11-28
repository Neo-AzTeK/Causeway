//! Title memory the GPU uses where it is, without a host copy
//! (`ID3D12Device3::OpenExistingHeapFromAddress` and the sized
//! `ID3D12Device13` form; idea from xcl-dev/xcl, our own code). Host copies
//! of buffers live in system memory (`crate::shadow`), so the GPU reads such
//! memory at the same cost; what goes away is the refresh, the snapshot and
//! the copy-back.
//!
//! An open heap pins its pages: Windows then refuses to free or unmap them
//! (the probes below). Only views the runtime maps are opened, since it
//! tells us before it unmaps one (`crate::watch::title_memory_remapped`),
//! and the copies over it are let go first ([`crate::shadow`]). Pools the
//! title frees itself stay copied.

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::core::Interface;

use crate::caps::ExistingHeaps;

/// One region of the address space, as `VirtualQuery` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) allocation: u64,
    pub(crate) committed: bool,
    /// A view of a section (`MEM_MAPPED`).
    pub(crate) mapped: bool,
}

/// The window `(start, len)` a heap is opened over for title memory at
/// `[at, at + n)`: the whole section view holding it, when it is committed
/// throughout (the runtime commits a view whole when it maps it), so that
/// one heap serves every buffer in it. `regions` are the view's, in order
/// from its start. None where `[at, at + n)` leaves the view or the device
/// opens no existing memory.
pub(crate) fn window(form: ExistingHeaps, at: u64, n: u64, regions: &[Region]) -> Option<(u64, u64)> {
    let end = at.checked_add(n.max(1))?;
    let first = regions.first()?;
    if form == ExistingHeaps::None || first.start != first.allocation {
        return None;
    }
    let mut covered = first.start;
    for r in regions {
        if r.start != covered || r.allocation != first.allocation || !r.committed || !r.mapped {
            return None;
        }
        covered = r.start + r.len;
    }
    (first.start <= at && end <= covered).then_some((first.start, covered - first.start))
}

/// The regions of the allocation holding `at`, from its start.
fn regions(at: u64) -> Vec<Region> {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_MAPPED, MEMORY_BASIC_INFORMATION, VirtualQuery};
    let query = |at: u64| {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: a query of our own address space into a local.
        let got = unsafe { VirtualQuery(at as *const _, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        (got != 0 && info.RegionSize != 0).then_some(Region {
            start: info.BaseAddress as u64,
            len: info.RegionSize as u64,
            allocation: info.AllocationBase as u64,
            committed: info.State == MEM_COMMIT,
            mapped: info.Type == MEM_MAPPED,
        })
    };
    let Some(allocation) = query(at).map(|r| r.allocation).filter(|&a| a != 0) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut next = allocation;
    while let Some(r) = query(next).filter(|r| r.allocation == allocation) {
        out.push(r);
        next = r.start + r.len;
    }
    out
}

/// Opened title memory: a buffer placed in a heap, showing title addresses
/// `[start, start + len)`.
pub(crate) struct Opened {
    pub(crate) buffer: ID3D12Resource,
    pub(crate) heap: ID3D12Heap,
    pub(crate) start: u64,
    pub(crate) len: u64,
    /// Whether the heap pins the title's view (opened from its address):
    /// Windows then refuses the title's unmap while the heap lives. A
    /// section opened whole pins only a view of our own, no title address.
    pub(crate) pins: bool,
    /// Where the buffer's first byte lies, when it is placed in a section
    /// opened whole: the section (the runtime's id for it) and the offset.
    pub(crate) place: Option<(usize, u64)>,
}

/// Whether title views are opened by address: `VICTORY_IN_PLACE=1`. Off by
/// default: in a world run the title unmapped views whose copies lists or
/// slots still held, Windows refused, and the title crashed.
fn wanted() -> bool {
    static WANTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WANTED.get_or_init(|| std::env::var("VICTORY_IN_PLACE").is_ok_and(|v| v == "1"))
}

/// Whether the physical-page sections title memory is mapped from are
/// opened whole, so the GPU uses that memory in place: on unless
/// `VICTORY_IN_PLACE=0`.
fn sections_wanted() -> bool {
    static WANTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WANTED.get_or_init(|| std::env::var("VICTORY_IN_PLACE").map_or(true, |v| v != "0"))
}

/// A section the runtime maps title memory from: our own handle to it, its
/// size, and its heap once asked for (`Some(None)`: the device refused),
/// opened over a view of the whole section we map (`view`, 0 for none).
/// Kept for the process's life: the heap holds the section's memory, and a
/// list may still use it after the title frees its pages.
struct Section {
    handle: windows::Win32::Foundation::HANDLE,
    size: u64,
    heap: Option<Option<ID3D12Heap>>,
    view: usize,
}

/// A title view of a section: its length and its offset in the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SectionView {
    len: u64,
    section: usize,
    offset: u64,
}

#[derive(Default)]
struct Sections {
    /// Title views by address.
    views: std::collections::BTreeMap<u64, SectionView>,
    /// Sections by the runtime's identity for them.
    sections: std::collections::HashMap<usize, Section>,
}

// SAFETY: a handle and a COM heap (agile), used under the lock.
unsafe impl Send for Sections {}

fn sections() -> std::sync::MutexGuard<'static, Sections> {
    static SECTIONS: std::sync::LazyLock<std::sync::Mutex<Sections>> = std::sync::LazyLock::new(Default::default);
    SECTIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The runtime mapped `[at, at + len)` of title memory from `offset` of a
/// section of `size` bytes (`handle`, which it may close later; `section`
/// names it while it lives). Called after the mapping.
pub fn title_memory_section(at: usize, len: usize, section: usize, handle: isize, size: u64, offset: u64) {
    if !sections_wanted() || len == 0 {
        return;
    }
    let mut s = sections();
    if let std::collections::hash_map::Entry::Vacant(entry) = s.sections.entry(section) {
        use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut own = HANDLE::default();
        // SAFETY: duplicates a live handle of this process into a local.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(handle as *mut _),
                GetCurrentProcess(),
                &mut own,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok.is_err() {
            return;
        }
        entry.insert(Section { handle: own, size, heap: None, view: 0 });
    }
    s.views.insert(at as u64, SectionView { len: len as u64, section, offset });
}

/// The runtime unmaps or remaps `[at, at + len)`: the section views there
/// go (buffers already placed keep their section's memory).
pub(crate) fn forget(at: u64, len: u64) {
    let mut s = sections();
    if s.views.is_empty() {
        return;
    }
    let end = at.saturating_add(len);
    let first = s.views.range(..=at).next_back().filter(|(b, v)| **b + v.len > at).map(|(b, _)| *b);
    let hit: Vec<u64> = first.into_iter().chain(s.views.range(at.saturating_add(1)..end).map(|(b, _)| *b)).collect();
    for b in hit {
        s.views.remove(&b);
    }
}

/// Largest run of title memory one placed buffer shows: buffers stay well
/// under every device's size limit, and finding a run walks its views.
const RUN_LIMIT: u64 = 1 << 30;

/// The run of title memory around `[at, at + n)` mapped from consecutive
/// offsets of one section: `(start, len, section, offset)`. None where the
/// range is not mapped that way throughout.
fn run(views: &std::collections::BTreeMap<u64, SectionView>, at: u64, n: u64) -> Option<(u64, u64, usize, u64)> {
    let end_wanted = at.checked_add(n.max(1))?;
    let (&base, v) = views.range(..=at).next_back().filter(|(b, v)| at < **b + v.len)?;
    let (section, mut start, mut offset, mut end) = (v.section, base, v.offset, base + v.len);
    while end - start < RUN_LIMIT {
        let Some((&b, p)) = views.range(..start).next_back() else { break };
        if b + p.len != start || p.section != section || p.offset + p.len != offset {
            break;
        }
        (start, offset) = (b, p.offset);
    }
    while end < end_wanted || end - start < RUN_LIMIT {
        let Some(next) = views.get(&end) else { break };
        if next.section != section || next.offset != offset + (end - start) {
            break;
        }
        end += next.len;
    }
    (end >= end_wanted).then_some((start, end - start, section, offset))
}

/// The heap of section `id`, opened on first use. Devices open committed
/// sections under 4 GiB, by handle and by address alike (probe
/// `sections_the_device_opens`); the runtime
/// makes physical-page sections so where the host can commit them.
///
/// Opened by address over a view of the whole section that we map and keep,
/// not by the section's handle (`OpenExistingHeapFromFileMapping`): a
/// driver was seen to keep a section opened by handle charged to system
/// commit after the process ended, heap released or not, until the machine
/// restarts; a view opened by address left nothing (probe
/// `section_commit_after_exit`). The heap pins our view, never a title one.
fn section_heap(device: &ID3D12Device3, s: &mut Section) -> Option<ID3D12Heap> {
    if let Some(heap) = &s.heap {
        return heap.clone();
    }
    let heap = section_view_heap(device, s);
    let heap = match heap {
        Ok(heap) => {
            tracing::info!(size = s.size, "title memory section opened for the GPU in place");
            Some(heap)
        }
        Err(e) => {
            refused(&e, 0, s.size);
            None
        }
    };
    s.heap = Some(heap.clone());
    heap
}

/// Maps section `s` whole into a view of ours (kept, recorded in `s.view`)
/// and opens the heap over it; a view not committed throughout is refused.
fn section_view_heap(device: &ID3D12Device3, s: &mut Section) -> windows::core::Result<ID3D12Heap> {
    use windows::Win32::Foundation::E_INVALIDARG;
    use windows_sys::Win32::System::Memory::{FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, UnmapViewOfFile};
    let len = usize::try_from(s.size).map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
    // SAFETY: maps our own handle to a live section, whole.
    let view = unsafe { MapViewOfFile(s.handle.0 as _, FILE_MAP_WRITE, 0, 0, len) }.Value as usize;
    if view == 0 {
        return Err(windows::core::Error::from_thread());
    }
    let unmap = || {
        // SAFETY: our view, which nothing else uses.
        unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut _ }) };
    };
    if window(ExistingHeaps::Whole, view as u64, s.size, &regions(view as u64)) != Some((view as u64, s.size)) {
        unmap();
        return Err(windows::core::Error::new(E_INVALIDARG, "section not committed throughout"));
    }
    // SAFETY: the start of our committed view, kept for the process's life.
    match unsafe { device.OpenExistingHeapFromAddress(view as *const _) } {
        Ok(heap) => {
            s.view = view;
            Ok(heap)
        }
        Err(e) => {
            unmap();
            Err(e)
        }
    }
}

/// Title memory around `[at, at + n)` used in place from its section: a
/// buffer placed at the run's offset in the section's heap.
fn open_section(device: &ID3D12Device, at: u64, n: u64) -> Option<Opened> {
    if !sections_wanted() {
        return None;
    }
    let device3 = device.cast::<ID3D12Device3>().ok()?;
    let mut s = sections();
    let (start, len, id, offset) = run(&s.views, at, n)?;
    let heap = section_heap(&device3, s.sections.get_mut(&id)?)?;
    drop(s);
    let buffer = placed(device, &heap, offset, len)?;
    Some(Opened { buffer, heap, start, len, pins: false, place: Some((id, offset)) })
}

/// The section page title address `at` shows now: the section and the
/// offset of the 64 KiB page holding `at`. None where no section view the
/// runtime mapped holds it.
fn shown_now(views: &std::collections::BTreeMap<u64, SectionView>, at: u64) -> Option<(usize, u64)> {
    let (&base, v) = views.range(..=at).next_back().filter(|(b, v)| at < **b + v.len)?;
    Some((v.section, v.offset + (at - base) / PAGE * PAGE))
}

/// The 64 KiB unit of sections, title views and remaps (the runtime's
/// physical page).
const PAGE: u64 = crate::watch::PAGE as u64;

/// One page a submitted list reaches through an in-place buffer whose title
/// address shows another page since the list was recorded: for the run,
/// the buffer's page (`old`, in our view of its section) holds what the
/// address shows (`put`), and its own bytes (`saved`) wait aside.
struct Lent {
    at: u64,
    old: *mut u8,
    saved: Vec<u8>,
    put: Vec<u8>,
}

/// What [`lend`] did, undone by [`Lending::give_back`] once the GPU is done.
#[derive(Default)]
pub(crate) struct Lending {
    pages: Vec<Lent>,
}

/// Before a submission runs, for the in-place buffers its lists hold and the
/// ranges they use there (`uses`): the console's GPU reads a title address
/// through the page tables when it runs, so a list recorded before the title
/// remapped an address sees the new page. Ours hold a buffer placed on the
/// section's old page (`crate::shadow::let_go`): for the run, that page
/// takes the bytes the address shows now, and what the GPU writes there goes
/// to the address afterwards ([`Lending::give_back`]).
///
/// Not page tables of the host's own (a reserved buffer whose 64 KiB tiles
/// map section pages, mapped again at each remap): D3D12 refuses tiles over
/// a heap opened from an address (`UpdateTileMappings` onto one gives
/// `DXGI_ERROR_INVALID_CALL` and removes the device).
///
/// shortcut: the old page is the title's memory too. Where another title
/// address shows it, the title's threads and the submission's other lists
/// reaching it there see the lent bytes during the run, and what they write
/// there meanwhile is lost (counted as `elsewhere` in the log line).
pub(crate) fn lend<'a>(
    uses: impl Iterator<Item = (&'a crate::shadow::Shadow, &'a std::collections::HashSet<(u64, u64)>)>,
) -> Lending {
    let mut lending = Lending::default();
    let wanted: Vec<(u64, usize, bool)> = {
        let s = sections();
        let mut wanted = Vec::new();
        let mut taken: std::collections::HashMap<usize, u64> = std::collections::HashMap::new();
        for (shadow, ranges) in uses {
            if !shadow.is_let_go() {
                continue;
            }
            let Some((start, len, section, offset)) = shadow.placed_run() else {
                continue;
            };
            let Some(view) = s.sections.get(&section).map(|x| x.view).filter(|&v| v != 0) else {
                continue;
            };
            let mut pages = std::collections::BTreeSet::new();
            for &(at, n) in ranges {
                let (from, to) = (at.max(start), at.saturating_add(n).min(start + len));
                let mut page = from / PAGE * PAGE;
                while page < to {
                    pages.insert(page);
                    page += PAGE;
                }
            }
            for page in pages {
                let page_offset = offset + (page - start);
                if shown_now(&s.views, page) == Some((section, page_offset)) {
                    continue;
                }
                let old = view + page_offset as usize;
                // Two buffers lending one page different bytes: the first
                // keeps it.
                if let Some(&other) = taken.get(&old) {
                    if other != page {
                        count(&COUNTS.conflicts);
                    }
                    continue;
                }
                taken.insert(old, page);
                // `page` shows another page now, so a view holding the old
                // one is at another address.
                let elsewhere = s
                    .views
                    .values()
                    .any(|v| v.section == section && v.offset <= page_offset && page_offset < v.offset + v.len);
                wanted.push((page, old, elsewhere));
            }
        }
        wanted
    };
    for (at, old, elsewhere) in wanted {
        let old = old as *mut u8;
        let mut saved = vec![0u8; PAGE as usize];
        let mut put = vec![0u8; PAGE as usize];
        // The address may show nothing now (the GPU would fault on the
        // console): the old page is left as it is then. Through the kernel,
        // which refuses a page the title decommitted instead of faulting.
        if !crate::host::read_kernel(old, saved.as_mut_ptr(), saved.len())
            || !crate::host::read_into(at as *const u8, &mut put)
            || !crate::host::read_kernel(put.as_ptr(), old, put.len())
        {
            count(&COUNTS.unmapped);
            continue;
        }
        if elsewhere {
            count(&COUNTS.elsewhere);
        }
        count(&COUNTS.lent);
        lending.pages.push(Lent { at, old, saved, put });
    }
    if !lending.pages.is_empty() {
        static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 32 {
            let get = |c: &std::sync::atomic::AtomicU64| c.load(std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                pages = lending.pages.len(),
                lent = get(&COUNTS.lent),
                elsewhere = get(&COUNTS.elsewhere),
                unmapped = get(&COUNTS.unmapped),
                conflicts = get(&COUNTS.conflicts),
                "a submission reaches remapped title pages through in-place buffers recorded before the remap: their old pages lent the new bytes"
            );
        }
    }
    lending
}

impl Lending {
    /// Once the GPU is done: what it wrote in each lent page goes to the
    /// title address (bytes that differ from those lent), and the page gets
    /// its own bytes back. Before the copy-backs and bottom-of-pipe writes.
    pub(crate) fn give_back(mut self) {
        self.undo(true);
    }

    fn undo(&mut self, gpu_ran: bool) {
        for page in self.pages.drain(..) {
            if gpu_ran {
                let mut now = vec![0u8; PAGE as usize];
                if crate::host::read_kernel(page.old, now.as_mut_ptr(), now.len()) {
                    for (from, to) in differing(&page.put, &now) {
                        let at = page.at as usize + from;
                        let _writing = crate::watch::gpu_writing(at, to - from);
                        crate::host::write_mapped(at as *mut u8, now[from..].as_ptr(), to - from);
                    }
                }
            }
            crate::host::read_kernel(page.saved.as_ptr(), page.old, page.saved.len());
        }
    }
}

impl Drop for Lending {
    /// Pages still lent (a submission that never ran) get their bytes back.
    fn drop(&mut self) {
        self.undo(false);
    }
}

/// The runs `[from, to)` where `a` and `b` differ.
fn differing(a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n {
        if a[i] == b[i] {
            i += 1;
            continue;
        }
        let from = i;
        while i < n && a[i] != b[i] {
            i += 1;
        }
        runs.push((from, i));
    }
    runs
}

/// What [`lend`] did over the run, for its log line.
struct Counts {
    lent: std::sync::atomic::AtomicU64,
    elsewhere: std::sync::atomic::AtomicU64,
    unmapped: std::sync::atomic::AtomicU64,
    conflicts: std::sync::atomic::AtomicU64,
}

static COUNTS: Counts = Counts {
    lent: std::sync::atomic::AtomicU64::new(0),
    elsewhere: std::sync::atomic::AtomicU64::new(0),
    unmapped: std::sync::atomic::AtomicU64::new(0),
    conflicts: std::sync::atomic::AtomicU64::new(0),
};

fn count(c: &std::sync::atomic::AtomicU64) {
    c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A buffer of `len` bytes at `offset` in an opened heap.
fn placed(device: &ID3D12Device, heap: &ID3D12Heap, offset: u64, len: u64) -> Option<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: len,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        // An opened heap is shared across adapters; shaders write buffers.
        Flags: D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: a buffer inside the heap, at a 64 KiB-aligned offset.
    let placed = unsafe { device.CreatePlacedResource(heap, offset, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut buffer) };
    match (placed, buffer) {
        (Ok(()), Some(buffer)) => Some(buffer),
        (r, _) => {
            refused(&r.err().unwrap_or_else(windows::core::Error::empty), offset, len);
            None
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The form a test's thread opens with (fake caps), whatever the switch.
    pub(crate) static FORM: std::cell::Cell<Option<ExistingHeaps>> = const { std::cell::Cell::new(None) };
}

/// Title memory around `[at, at + n)` opened for the GPU, where the device
/// and the memory allow it; none where a host copy is needed.
pub(crate) fn open(at: u64, n: u64) -> Option<Opened> {
    let host = crate::host::host()?;
    #[cfg(test)]
    let forced = FORM.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let forced = None;
    if forced.is_none()
        && host.caps.existing_heaps != ExistingHeaps::None
        && let Some(opened) = open_section(&host.device, at, n)
    {
        return Some(opened);
    }
    let form = match forced {
        Some(form) => form,
        None if wanted() => host.caps.existing_heaps,
        None => return None,
    };
    let (start, len) = window(form, at, n, &regions(at))?;
    let heap: windows::core::Result<ID3D12Heap> = match form {
        ExistingHeaps::None => return None,
        // SAFETY: committed memory of one section view the runtime mapped,
        // which it unmaps only after the copies over it are let go.
        ExistingHeaps::Part => unsafe {
            host.device.cast::<ID3D12Device13>().ok()?.OpenExistingHeapFromAddress1(start as *const _, len as usize)
        },
        // SAFETY: as above, from the allocation's start.
        ExistingHeaps::Whole => unsafe { host.device.cast::<ID3D12Device3>().ok()?.OpenExistingHeapFromAddress(start as *const _) },
    };
    let heap = match heap {
        Ok(heap) => heap,
        Err(e) => {
            refused(&e, start, len);
            return None;
        }
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: len,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        // An opened heap is shared across adapters; shaders write buffers.
        Flags: D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: a buffer as large as the heap, at its start.
    let placed = unsafe { host.device.CreatePlacedResource(&heap, 0, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut buffer) };
    match (placed, buffer) {
        (Ok(()), Some(buffer)) => Some(Opened { buffer, heap, start, len, pins: true, place: None }),
        (r, _) => {
            refused(&r.err().unwrap_or_else(windows::core::Error::empty), start, len);
            None
        }
    }
}

fn refused(e: &windows::core::Error, start: u64, len: u64) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing::warn!(
            start = format_args!("{start:#x}"),
            len,
            error = %e,
            "title memory not opened in place: such buffers are copied (logged once)"
        )
    });
}

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::core::Interface;
    use windows_sys::Win32::System::Memory::{
        GetWriteWatch, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, ResetWriteWatch, VirtualAlloc,
        VirtualFree,
    };

    const MIB: usize = 1 << 20;
    const MEM_WRITE_WATCH: u32 = 0x20_0000;

    fn buffer_over(device: &ID3D12Device, heap: &ID3D12Heap, len: usize) -> windows::core::Result<ID3D12Resource> {
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: len as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER,
            ..Default::default()
        };
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: a buffer placed at the start of our heap, as large as it.
        unsafe { device.CreatePlacedResource(heap, 0, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut made) }?;
        Ok(made.expect("made"))
    }

    /// A write-watched, wholly committed allocation (as the runtime's GPU
    /// pools are made) opened as a heap: the GPU copies one half into the
    /// other, and the CPU reads the result in the same memory. The CPU's own
    /// writes stay watched.
    #[test]
    fn title_style_memory_is_used_by_the_gpu_in_place() {
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 2 * MIB;
        // SAFETY: a fresh allocation of our own, released below.
        let at = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) };
        assert!(!at.is_null());
        let words = at.cast::<u32>();
        for i in 0..(64 << 10) / 4 {
            // SAFETY: inside the allocation.
            unsafe { words.add(i).write(0xC0DE_0000 | i as u32) };
        }
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: the start of a committed allocation that outlives the heap.
        let heap: windows::core::Result<ID3D12Heap> = unsafe { device3.OpenExistingHeapFromAddress(at) };
        let heap = match heap {
            Ok(heap) => heap,
            Err(e) => panic!("the device does not open title-style memory: {e}"),
        };
        // SAFETY: plain query of our heap.
        let desc = unsafe { heap.GetDesc() };
        eprintln!(
            "opened {} bytes: heap type {:?}, page {:?}, pool {:?}, flags {:?}",
            desc.SizeInBytes,
            desc.Properties.Type,
            desc.Properties.CPUPageProperty,
            desc.Properties.MemoryPoolPreference,
            desc.Flags
        );
        assert_eq!(desc.SizeInBytes as usize, len);
        let buffer = buffer_over(&host.device, &heap, len).expect("a buffer placed over it");
        // What opening the heap marked written (the driver pins the pages)
        // is cleared, then the CPU writes one page again.
        let watched = |at: *mut core::ffi::c_void| {
            let mut pages = [std::ptr::null_mut::<core::ffi::c_void>(); 512];
            let (mut count, mut granularity) = (pages.len(), 0u32);
            // SAFETY: room for `count` addresses; no reset.
            let r = unsafe { GetWriteWatch(0, at, len, pages.as_mut_ptr(), &mut count, &mut granularity) };
            assert_eq!(r, 0);
            pages[..count].iter().map(|&p| (p as usize - at as usize) / 4096).collect::<Vec<usize>>()
        };
        eprintln!("write watch after opening: {} pages", watched(at).len());
        // SAFETY: our watched allocation.
        assert_eq!(unsafe { ResetWriteWatch(at, len) }, 0);
        // SAFETY: inside the allocation.
        unsafe { words.add(5).write(0xC0DE_0005) };
        let list = host.new_list().unwrap();
        // SAFETY: an open list; both ranges inside the buffer, apart.
        unsafe { list.list.CopyBufferRegion(&buffer, MIB as u64, &buffer, 0, 64 << 10) };
        list.close();
        host.execute(&[&list]);
        let half = MIB / 4;
        for i in [0usize, 1, 4095, (64 << 10) / 4 - 1] {
            // SAFETY: inside the allocation; the GPU finished (execute waits).
            let got = unsafe { words.add(half + i).read() };
            assert_eq!(got, 0xC0DE_0000 | i as u32, "dword {i} of the GPU's copy");
        }
        // The CPU's write after the reset is seen; whether the GPU's are is
        // only reported.
        let seen = watched(at);
        assert!(seen.contains(&0), "{seen:?}");
        let gpu_seen = seen.iter().filter(|&&p| p >= MIB / 4096).count();
        eprintln!("write watch after the copy: {} pages, {gpu_seen} in the GPU's 16", seen.len());
        drop(buffer);
        drop(heap);
        // SAFETY: ours; nothing uses it now.
        unsafe { VirtualFree(at, 0, MEM_RELEASE) };
    }

    /// Part of an allocation: the plain form refuses an address that is not
    /// an allocation's start (xcl's note); the sized form, where the device
    /// has it, is asked for the second megabyte only.
    #[test]
    fn part_of_an_allocation_is_opened_or_refused() {
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 4 * MIB;
        // SAFETY: a fresh allocation of our own, released below.
        let at = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) };
        assert!(!at.is_null());
        let inner = at.wrapping_byte_add(MIB);
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: inside a committed allocation that outlives the heap.
        let plain: windows::core::Result<ID3D12Heap> = unsafe { device3.OpenExistingHeapFromAddress(inner) };
        // SAFETY: plain query.
        let plain_size = plain.as_ref().map(|h| unsafe { h.GetDesc() }.SizeInBytes);
        eprintln!("plain form at +1 MiB: {plain_size:?}");
        match host.device.cast::<ID3D12Device13>() {
            Ok(device13) => {
                // SAFETY: as above, one megabyte inside the allocation.
                let sized: windows::core::Result<ID3D12Heap> = unsafe { device13.OpenExistingHeapFromAddress1(inner, MIB) };
                // SAFETY: plain query.
                let size = sized.as_ref().map(|h| unsafe { h.GetDesc() }.SizeInBytes);
                eprintln!("sized form, 1 MiB at +1 MiB: {size:?}");
            }
            Err(e) => eprintln!("no ID3D12Device13: {e}"),
        }
        drop(plain);
        // SAFETY: ours; nothing uses it now.
        unsafe { VirtualFree(at, 0, MEM_RELEASE) };
    }

    /// Physical pages as the runtime makes them (`victory_hle` xmem): a
    /// pagefile section reserved, its view put in a placeholder, its pages
    /// committed on use. Whether the device opens such a view decides if
    /// most of the world's copies could go (they copy views like this).
    #[test]
    fn a_physical_page_view_is_opened_or_refused() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE_PLACEHOLDER,
            MapViewOfFile3, SEC_RESERVE, UnmapViewOfFile2, VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 2 * MIB;
        // SAFETY: plain section creation, closed below.
        let section =
            unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE | SEC_RESERVE, 0, len as u32, std::ptr::null()) };
        assert!(!section.is_null());
        // SAFETY: a placeholder of our own, replaced by the view.
        let place = unsafe {
            VirtualAlloc2(GetCurrentProcess(), std::ptr::null(), len, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, 0x01, std::ptr::null_mut(), 0)
        };
        assert!(!place.is_null());
        // SAFETY: the whole placeholder replaced by the whole section.
        let view = unsafe {
            MapViewOfFile3(section, GetCurrentProcess(), place, 0, len, MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE, std::ptr::null_mut(), 0)
        };
        assert_eq!(view.Value, place);
        // SAFETY: commits the view's pages, as a first use does.
        assert!(!unsafe { VirtualAlloc(place, len, MEM_COMMIT, PAGE_READWRITE) }.is_null());
        // SAFETY: inside the committed view.
        unsafe { place.cast::<u32>().write(0xC0DE_0001) };
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: the view outlives the heaps.
        let plain: windows::core::Result<ID3D12Heap> = unsafe { device3.OpenExistingHeapFromAddress(place) };
        eprintln!("plain form on a section view: {:?}", plain.as_ref().map(|_| ()).map_err(|e| e.code()));
        let sized = host.device.cast::<ID3D12Device13>().ok().map(|d13| {
            // SAFETY: as above.
            let heap: windows::core::Result<ID3D12Heap> = unsafe { d13.OpenExistingHeapFromAddress1(place, len) };
            heap
        });
        eprintln!(
            "sized form on a section view: {:?}",
            sized.as_ref().map(|r| r.as_ref().map(|_| ()).map_err(|e| e.code()))
        );
        // A heap that opened is used: the GPU copies dword 0 to 1 MiB.
        if let Some(heap) = plain.ok().or_else(|| sized.and_then(Result::ok)) {
            let buffer = buffer_over(&host.device, &heap, len).expect("a buffer placed over it");
            let list = host.new_list().unwrap();
            // SAFETY: an open list; both ranges inside the buffer.
            unsafe { list.list.CopyBufferRegion(&buffer, MIB as u64, &buffer, 0, 4) };
            list.close();
            host.execute(&[&list]);
            // SAFETY: inside the view; the GPU finished.
            let got = unsafe { place.cast::<u32>().wrapping_byte_add(MIB).read() };
            assert_eq!(got, 0xC0DE_0001, "the GPU's copy in the view");
            eprintln!("the GPU used the section view in place");
        }
        // SAFETY: ours, unused now.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), view, MEM_PRESERVE_PLACEHOLDER);
            VirtualFree(place, 0, MEM_RELEASE);
            CloseHandle(section);
        }
    }

    /// A list recorded over a title page used in place from its section,
    /// submitted after the runtime mapped another page of the section there:
    /// it reads and writes what the address shows then (the console's GPU
    /// reads addresses through the page tables when it runs), and the old
    /// page keeps its bytes.
    #[test]
    fn a_list_recorded_before_a_remap_sees_the_new_page() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE_PLACEHOLDER,
            MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, MapViewOfFile3, UnmapViewOfFile, UnmapViewOfFile2, VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        const P: usize = super::PAGE as usize;
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        // Two physical pages, 0x11s and 0x22s, as the runtime makes them.
        // SAFETY: plain section creation (committed whole), kept by the views.
        let section =
            unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, 2 * P as u32, std::ptr::null()) };
        assert!(!section.is_null());
        // SAFETY: our own view of the section, to fill and inspect it.
        let own = unsafe { MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * P) }.Value.cast::<u8>();
        assert!(!own.is_null());
        // SAFETY: inside our view.
        unsafe {
            std::ptr::write_bytes(own, 0x11, P);
            std::ptr::write_bytes(own.add(P), 0x22, P);
        }
        // The title's address: a placeholder the first page is mapped over.
        // SAFETY: a placeholder of our own, replaced by the view.
        let at = unsafe {
            VirtualAlloc2(GetCurrentProcess(), std::ptr::null(), P, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, 0x01, std::ptr::null_mut(), 0)
        };
        assert!(!at.is_null());
        let map_page = |offset: usize| {
            // SAFETY: one section page over our placeholder.
            let v = unsafe {
                MapViewOfFile3(section, GetCurrentProcess(), at, offset as u64, P, MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE, std::ptr::null_mut(), 0)
            };
            assert_eq!(v.Value, at);
            super::title_memory_section(at as usize, P, section as usize, section as isize, 2 * P as u64, offset as u64);
        };
        map_page(0);
        let address = at as u64;
        let Some(copy) = crate::shadow::find(address, 512) else {
            panic!("no copy");
        };
        if copy.placed_run().is_none() {
            eprintln!("skipped: the device does not open sections in place");
            return;
        }
        assert_eq!(copy.placed_run().map(|r| (r.0, r.3)), Some((address, 0)), "placed on the first page");
        let (out, out_data) = host.shared_buffer(P as u64).expect("a host buffer");
        let (src, src_data) = host.shared_buffer(P as u64).expect("a host buffer");
        // SAFETY: inside the host buffers.
        unsafe {
            std::ptr::write_bytes(out_data, 0, P);
            std::ptr::write_bytes(src_data, 0x33, P);
        }
        // Recorded now: the list reads bytes 0..256 and writes 256..512.
        let list = host.new_list().unwrap();
        copy.refresh(&list, address, 256);
        copy.gpu_writes(&list, address + 256, 256, false);
        // SAFETY: an open list; ranges inside the buffers.
        unsafe {
            list.list.CopyBufferRegion(&out, 0, &copy.buffer, 0, 256);
            list.list.CopyBufferRegion(&copy.buffer, 256, &src, 0, 256);
        }
        list.close();
        // The runtime maps the second page there, as xmem does.
        crate::watch::title_memory_remapped(at as usize, P);
        // SAFETY: the view over our placeholder, back to a placeholder.
        unsafe { UnmapViewOfFile2(GetCurrentProcess(), MEMORY_MAPPED_VIEW_ADDRESS { Value: at }, MEM_PRESERVE_PLACEHOLDER) };
        map_page(P);
        crate::watch::title_memory_remapped(at as usize, P);
        assert!(copy.is_let_go());
        host.execute(&[&list]);
        // SAFETY: inside the buffers and views; the GPU finished.
        let (read, wrote, old_read, old_wrote) = unsafe { (*out_data, *at.cast::<u8>().add(256), *own, *own.add(256)) };
        assert_eq!(read, 0x22, "read what the address shows now");
        assert_eq!(wrote, 0x33, "wrote where the address shows now");
        assert_eq!((old_read, old_wrote), (0x11, 0x11), "the old page keeps its bytes");
        drop((list, copy, out, src));
        crate::shadow::let_go(at as u64, P as u64);
        // SAFETY: ours; the section's heap keeps its own view.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), MEMORY_MAPPED_VIEW_ADDRESS { Value: at }, MEM_PRESERVE_PLACEHOLDER);
            VirtualFree(at, 0, MEM_RELEASE);
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: own.cast() });
            CloseHandle(section);
        }
    }

    /// The title unmaps a view while a heap opened over it pins its pages,
    /// then maps other pages there: whether the unmap is refused, and
    /// whether the GPU's writes through the old heap land in the new view
    /// (they must not be relied on: the in-place copy is retired then).
    #[test]
    fn a_view_unmapped_under_an_open_heap() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE_PLACEHOLDER,
            MapViewOfFile3, SEC_RESERVE, UnmapViewOfFile2, VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let Ok(device13) = host.device.cast::<ID3D12Device13>() else {
            eprintln!("skipped: no ID3D12Device13");
            return;
        };
        let len = 2 * MIB;
        let section = || {
            // SAFETY: plain section creation, closed below.
            let s = unsafe {
                CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE | SEC_RESERVE, 0, len as u32, std::ptr::null())
            };
            assert!(!s.is_null());
            s
        };
        let (first, second) = (section(), section());
        // SAFETY: a placeholder of our own, replaced by the views.
        let place = unsafe {
            VirtualAlloc2(GetCurrentProcess(), std::ptr::null(), len, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, 0x01, std::ptr::null_mut(), 0)
        };
        let map = |s| {
            // SAFETY: the whole placeholder replaced by a whole section; its
            // pages committed as a first use does.
            unsafe {
                let view = MapViewOfFile3(s, GetCurrentProcess(), place, 0, len, MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE, std::ptr::null_mut(), 0);
                assert_eq!(view.Value, place);
                assert!(!VirtualAlloc(place, len, MEM_COMMIT, PAGE_READWRITE).is_null());
                view
            }
        };
        let view = map(first);
        // SAFETY: inside the committed view.
        unsafe { place.cast::<u32>().write(0xC0DE_0001) };
        // SAFETY: the view outlives the heap's use below or is reported.
        let heap: ID3D12Heap = unsafe { device13.OpenExistingHeapFromAddress1(place, len) }.expect("opened");
        let buffer = buffer_over(&host.device, &heap, len).expect("a buffer placed over it");
        // SAFETY: our view; the heap still pins it.
        let unmapped = unsafe { UnmapViewOfFile2(GetCurrentProcess(), view, MEM_PRESERVE_PLACEHOLDER) };
        eprintln!("unmap under an open heap: {}", if unmapped != 0 { "done" } else { "refused" });
        if unmapped == 0 {
            drop(buffer);
            drop(heap);
            // SAFETY: ours, unused now.
            unsafe { UnmapViewOfFile2(GetCurrentProcess(), view, MEM_PRESERVE_PLACEHOLDER) };
        } else {
            let again = map(second);
            // SAFETY: inside the committed view.
            unsafe { place.cast::<u32>().write(0xC0DE_0002) };
            let list = host.new_list().unwrap();
            // SAFETY: an open list; both ranges inside the buffer.
            unsafe { list.list.CopyBufferRegion(&buffer, MIB as u64, &buffer, 0, 4) };
            list.close();
            host.execute(&[&list]);
            // SAFETY: inside the new view; the GPU finished.
            let got = unsafe { place.cast::<u32>().wrapping_byte_add(MIB).read() };
            eprintln!("the GPU's copy through the old heap, read in the new view: {got:#x} (0 = old pages)");
            drop(buffer);
            drop(heap);
            // SAFETY: ours, unused now.
            unsafe { UnmapViewOfFile2(GetCurrentProcess(), again, MEM_PRESERVE_PLACEHOLDER) };
        }
        // SAFETY: ours, unused now.
        unsafe {
            VirtualFree(place, 0, MEM_RELEASE);
            CloseHandle(first);
            CloseHandle(second);
        }
    }

    /// The title frees a pool while a heap opened over it pins it: whether
    /// the free is refused.
    #[test]
    fn a_pool_freed_under_an_open_heap() {
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 2 * MIB;
        // SAFETY: a fresh allocation of our own.
        let at = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) };
        assert!(!at.is_null());
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: the start of a committed allocation.
        let heap: ID3D12Heap = unsafe { device3.OpenExistingHeapFromAddress(at) }.expect("opened");
        // SAFETY: ours; the heap still pins it.
        let freed = unsafe { VirtualFree(at, 0, MEM_RELEASE) };
        eprintln!("free under an open heap: {}", if freed != 0 { "done" } else { "refused" });
        drop(heap);
        if freed == 0 {
            // SAFETY: ours, unused now.
            unsafe { VirtualFree(at, 0, MEM_RELEASE) };
        }
    }

    use super::{ExistingHeaps, FORM, Region, window};

    fn region(start: u64, len: u64, allocation: u64) -> Region {
        Region { start, len, allocation, committed: true, mapped: true }
    }

    /// A view committed throughout is opened whole for any memory inside
    /// it; a range leaving it, a gap, a region not committed or not a view,
    /// or a device with no form opens nothing.
    #[test]
    fn the_window_is_the_whole_view() {
        let v = 0x1_0000_0000u64;
        let view = [region(v, 0x10000, v), region(v + 0x10000, 0x30000, v)];
        for form in [ExistingHeaps::Part, ExistingHeaps::Whole] {
            assert_eq!(window(form, v + 0x12345, 0x100, &view), Some((v, 0x40000)));
            assert_eq!(window(form, v, 0x40000, &view), Some((v, 0x40000)));
            assert_eq!(window(form, v + 0x3ff00, 0x101, &view), None, "leaves the view");
        }
        assert_eq!(window(ExistingHeaps::None, v, 16, &view), None);
        assert_eq!(window(ExistingHeaps::Part, v, 16, &[]), None);
        let gap = [region(v, 0x10000, v), region(v + 0x20000, 0x10000, v)];
        assert_eq!(window(ExistingHeaps::Part, v, 16, &gap), None);
        let reserved = [region(v, 0x10000, v), Region { committed: false, ..region(v + 0x10000, 0x10000, v) }];
        assert_eq!(window(ExistingHeaps::Part, v, 16, &reserved), None, "a part not committed");
        let private = [Region { mapped: false, ..region(v, 0x10000, v) }];
        assert_eq!(window(ExistingHeaps::Part, v, 16, &private), None, "not a view: the title frees it itself");
        let inner = [region(v + 0x10000, 0x10000, v)];
        assert_eq!(window(ExistingHeaps::Part, v + 0x10000, 16, &inner), None, "not from the view's start");
    }

    /// A committed pagefile view of `len` bytes, as the runtime maps the
    /// title's physical pages.
    fn view(len: usize) -> usize {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile};
        // SAFETY: a pagefile section mapped once; the view keeps it.
        unsafe {
            let section =
                CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, len as u32, std::ptr::null());
            assert!(!section.is_null());
            let at = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, len).Value as usize;
            windows_sys::Win32::Foundation::CloseHandle(section);
            assert_ne!(at, 0);
            at
        }
    }

    fn unmap(at: usize) -> bool {
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        // SAFETY: a view of ours.
        unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: at as *mut _ }) != 0 }
    }

    /// A title buffer in a view: one in-place
    /// copy serves the buffer and loose memory of the view; nothing is
    /// refreshed or copied back; the GPU's copy lands in title memory; and
    /// once let go (the runtime is about to unmap), the view unmaps. Both
    /// forms (sized, and whole allocations).
    #[test]
    fn a_buffer_in_a_view_is_used_in_place() {
        for form in [ExistingHeaps::Part, ExistingHeaps::Whole] {
            in_place_with(form);
        }
    }

    fn in_place_with(form: ExistingHeaps) {
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 2 * MIB;
        let at = view(len);
        let buffer = (at + 0x1000) as u64;
        FORM.set(Some(form));
        crate::shadow::register(0x1a_5eed, buffer, 0x2000);
        let copy = crate::shadow::find(buffer, 16).expect("a copy");
        let other = crate::shadow::find((at + MIB) as u64, 16).expect("loose memory of the view");
        FORM.set(None);
        assert!(copy.in_place());
        assert!(std::sync::Arc::ptr_eq(&copy, &other), "one copy per view");
        assert_eq!(copy.range(), (at as u64, len as u64));
        let words = buffer as *mut u32;
        for i in 0..64 {
            // SAFETY: inside the view.
            unsafe { words.add(i).write(0xC0DE_0000 | i as u32) };
        }
        assert!(!copy.stale(buffer as usize, 256), "nothing to refresh");
        assert_eq!(copy.update(buffer as usize, 256), 0);
        let list = host.new_list().unwrap();
        let (from, to) = (copy.offset(buffer), copy.offset(buffer + 0x10000));
        // SAFETY: an open list; both ranges inside the view's buffer.
        unsafe { list.list.CopyBufferRegion(&copy.buffer, to, &copy.buffer, from, 256) };
        list.close();
        host.execute(&[&list]);
        assert_eq!(copy.copy_back(buffer as usize + 0x10000, 256), 0, "nothing to copy back");
        for i in [0usize, 1, 63] {
            // SAFETY: inside the view; the GPU finished.
            let got = unsafe { ((buffer + 0x10000) as *const u32).add(i).read() };
            assert_eq!(got, 0xC0DE_0000 | i as u32, "dword {i} of the GPU's copy in title memory");
        }
        crate::shadow::unregister(0x1a_5eed, buffer);
        crate::shadow::let_go(at as u64, len as u64);
        drop((copy, other));
        assert!(unmap(at), "the view unmaps once its copy let go");
    }

    /// A device that opens no existing memory (fake caps): the buffer gets
    /// a host copy, as before.
    #[test]
    fn without_the_form_a_buffer_is_copied() {
        if crate::host::host().is_none() {
            eprintln!("skipped: no host GPU");
            return;
        }
        let len = 2 * MIB;
        let at = view(len);
        let buffer = (at + 0x1000) as u64;
        FORM.set(Some(ExistingHeaps::None));
        crate::shadow::register(0x1a_5eee, buffer, 0x2000);
        let copy = crate::shadow::find(buffer, 16).expect("a copy");
        FORM.set(None);
        assert!(!copy.in_place());
        assert_eq!(copy.range(), (buffer, 0x2000));
        crate::shadow::unregister(0x1a_5eee, buffer);
        drop(copy);
        assert!(unmap(at));
    }

    /// Which sections this device opens whole: prints each kind's answer
    /// (a probe, run by hand with `--ignored --nocapture`).
    #[test]
    #[ignore]
    fn sections_the_device_opens() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, PAGE_READWRITE, SEC_COMMIT, SEC_RESERVE};
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let device = host.device.cast::<ID3D12Device3>().unwrap();
        for (what, kind, size) in [
            ("reserved 64 MiB", SEC_RESERVE, 64u64 << 20),
            ("committed 64 MiB", SEC_COMMIT, 64 << 20),
            ("committed 1 GiB", SEC_COMMIT, 1 << 30),
            ("committed 4 GiB + 64 KiB", SEC_COMMIT, (4 << 30) + (64 << 10)),
        ] {
            // SAFETY: a plain pagefile-backed section.
            let h = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE | kind, (size >> 32) as u32, size as u32, std::ptr::null()) };
            assert!(!h.is_null(), "{what}: no section");
            // SAFETY: a live section handle.
            let heap: windows::core::Result<ID3D12Heap> = unsafe { device.OpenExistingHeapFromFileMapping(windows::Win32::Foundation::HANDLE(h)) };
            eprintln!("{what}, by handle: {:?}", heap.as_ref().map(|_| "opened").map_err(|e| e.message()));
            drop(heap);
            // The runtime's path: a view of ours, opened by address.
            let mut s = super::Section { handle: windows::Win32::Foundation::HANDLE(h), size, heap: None, view: 0 };
            let heap = super::section_view_heap(&device, &mut s);
            eprintln!("{what}, by address: {:?}", heap.as_ref().map(|_| "opened").map_err(|e| e.message()));
            drop(heap);
            if s.view != 0 {
                use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
                // SAFETY: the probe's own view, its heap released.
                unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: s.view as *mut _ }) };
            }
            // SAFETY: our handle.
            unsafe { CloseHandle(h) };
        }
    }

    /// System commit in use and physical memory available, in MiB.
    fn memory_mib() -> (u64, u64) {
        use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
        // SAFETY: plain integers, all-zero valid.
        let mut info: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a sized out struct.
        assert!(unsafe { GetPerformanceInfo(&mut info, std::mem::size_of_val(&info) as u32) } != 0);
        ((info.CommitTotal * info.PageSize) as u64 >> 20, (info.PhysicalAvailable * info.PageSize) as u64 >> 20)
    }

    fn commit_mib() -> u64 {
        memory_mib().0
    }

    fn phys_free_mib() -> u64 {
        memory_mib().1
    }

    const LEAK_CHILD: &str = "VICTORY_TEST_SECTION_LEAK_CHILD";

    /// How much of a 1 GiB committed section a process leaves charged to
    /// system commit after it ends, for each way it opens the section for
    /// the GPU and ends (a probe, run by hand with `--ignored --nocapture`).
    #[test]
    #[ignore]
    fn section_commit_after_exit() {
        use windows_sys::Win32::Foundation::{HANDLE as RAW, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, SEC_COMMIT};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        const SIZE: usize = 1 << 30;
        if let Ok(how) = std::env::var(LEAK_CHILD) {
            let Some(host) = crate::host::host() else { std::process::exit(3) };
            let device = host.device.cast::<ID3D12Device3>().unwrap();
            // SAFETY: a plain pagefile-backed section.
            let h: RAW = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE | SEC_COMMIT, 0, SIZE as u32, std::ptr::null()) };
            assert!(!h.is_null());
            let mut heap: Option<ID3D12Heap> = None;
            if how != "runtime" {
                eprintln!("{how}: physical memory free before the open {} MiB", phys_free_mib());
            }
            if how == "runtime" {
                // The runtime's path: a title view of the section, then a buffer in it.
                // SAFETY: maps our section whole.
                let at = unsafe { MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, SIZE) }.Value as usize;
                assert_ne!(at, 0);
                let free = || phys_free_mib();
                let before = free();
                super::title_memory_section(at, SIZE, 0x5ec7, h as isize, SIZE as u64, 0);
                let opened = super::open(at as u64, 0x1_0000).expect("opened in place");
                assert!(!opened.pins, "a section heap pins no title view");
                eprintln!("runtime: physical memory taken by the open {} MiB", before as i64 - free() as i64);
                std::mem::forget(opened);
            } else if how.starts_with("file") {
                // SAFETY: a live section handle.
                heap = Some(unsafe { device.OpenExistingHeapFromFileMapping(windows::Win32::Foundation::HANDLE(h)) }.unwrap());
            } else if how.starts_with("address") {
                // SAFETY: maps our section whole.
                let at = unsafe { MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, SIZE) }.Value;
                assert!(!at.is_null());
                // SAFETY: the start of a live view.
                heap = Some(unsafe { device.OpenExistingHeapFromAddress(at) }.unwrap());
            }
            let buffer = heap.as_ref().map(|heap| buffer_over(&host.device, heap, SIZE).unwrap());
            if !how.ends_with("released") && how != "none" && how != "runtime" {
                eprintln!("{how}: physical memory free after the open {} MiB", phys_free_mib());
            }
            if how.ends_with("released") {
                drop(buffer);
                drop(heap);
            } else {
                std::mem::forget((buffer, heap));
            }
            // SAFETY: ends this process as the runtime's `end` does.
            unsafe { TerminateProcess(GetCurrentProcess(), 0) };
            unreachable!();
        }
        let name = "inplace::tests::section_commit_after_exit";
        let mut left_by_runtime = 0;
        for how in ["none", "file", "file-released", "address", "address-released", "runtime"] {
            let before = commit_mib();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--ignored", "--nocapture"])
                .env(LEAK_CHILD, how)
                .status()
                .unwrap();
            if status.code() == Some(3) {
                eprintln!("skipped: no host GPU");
                return;
            }
            assert_eq!(status.code(), Some(0), "{how}: the child failed");
            std::thread::sleep(std::time::Duration::from_secs(3));
            let left = commit_mib() as i64 - before as i64;
            eprintln!("{how}: commit left {left} MiB");
            if how == "runtime" {
                left_by_runtime = left;
            }
        }
        assert!(left_by_runtime < 256, "the runtime's section heap left {left_by_runtime} MiB of commit after exit");
    }
}
