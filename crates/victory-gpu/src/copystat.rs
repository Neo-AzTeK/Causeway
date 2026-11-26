//! Diagnostics: where the submission copies of title memory go (which
//! ranges, how often, of what memory) and what limits their rate, logged
//! with the frame-rate line. `VICTORY_COPY_MODE=plain` copies without the
//! mapped-page guard, `none` skips the copies (both for timing only).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Counters since the last line.
pub(crate) enum C {
    Submissions,
    Copies,
    CopyBytes,
    Refreshes,
    RefreshBytes,
    Skipped,
    SkippedBytes,
    Runs,
    RunUs,
    Queries,
    QueryUs,
    MoveUs,
    Unmapped,
    /// Write-watch askings and their time.
    Watches,
    WatchUs,
    /// Bytes of watched ranges checked, and of those found unchanged.
    Checked,
    Unchanged,
    /// Copies that met an unmapped page.
    Fallbacks,
    /// Writes taken by the guard on view memory.
    GuardFaults,
    /// Texture bytes detiled after a watched write; hashed; detiled after a
    /// hash change.
    TexWatched,
    TexHashed,
    TexChanged,
    /// Copy-back: bytes of pieces the GPU changed, and microseconds in the
    /// comparison, title reads, merges, title writes and the final watch.
    BackDiffBytes,
    BackCompareUs,
    BackReadUs,
    BackMergeUs,
    BackWriteUs,
    BackWatchUs,
    /// Time in the kernel's write-watch answers, in the view check of the
    /// guard, in protecting; bytes and time of copies of memory neither
    /// watched nor guarded (copied whole).
    KernelUs,
    ViewQueryUs,
    ProtectUs,
    WholeBytes,
    WholeUs,
    /// Snapshots of ranges shaders write (bytes, time); page faults while
    /// preparing submissions and while copying back.
    SnapshotBytes,
    SnapshotUs,
    FaultsPrep,
    FaultsBack,
    /// Pages marked changed because a write landed on another mapping of
    /// them.
    AliasMarks,
    /// Time in the guard handler; protection changes making pages read-only
    /// (calls, pages) and giving write access back (pages).
    HandlerUs,
    ProtectCalls,
    ProtectPages,
    OpenPages,
    /// Tracked pages written (distinct, per frame), and of those the ones
    /// written the frame before too.
    WrittenPages,
    WrittenAgain,
    /// In change checks: time asking the runtime for other mappings, and
    /// waiting for the guard lock.
    AliasUs,
    GuardWaitUs,
    /// Preparing submissions: refresh batches done (copies and change
    /// checks); time in each kind of step (a step that flushes the batch
    /// counts that too); texture uploads for heaps; state preludes.
    FlushUs,
    StepRefreshUs,
    StepCopyUs,
    StepWorkUs,
    StepGpuWriteUs,
    StepTextureUs,
    StepHeapUs,
    StepMemoryUs,
    UploadsUs,
    PreludeUs,
    /// Copying back: bytes of copies compared with their snapshots, and
    /// the time.
    CompareBytes,
    CompareUs,
    /// Copying back pages shaders marked as written through views: pages
    /// compared, bytes written back, the time; pages that kept the title's
    /// bytes (it wrote them, their bytes from before unknown); pages whose
    /// bytes the title's first write saved.
    MarkedPages,
    MarkedBytes,
    MarkedUs,
    MarkedKept,
    SavedPages,
    /// Preparing submissions, the parts outside the steps: title heaps
    /// mirrored, host textures touched, the last refresh batch, residency,
    /// written-pages read-backs; and the whole preparing.
    SyncHeapsUs,
    TouchUs,
    LastFlushUs,
    ResidentUs,
    MarksUs,
    PrepareUs,
    /// Title heap slots synced (their page written), and of all mirrored
    /// slots those made again.
    SyncSlots,
    SyncMade,
    /// Refresh batches: time bringing ranges up to date where no shader
    /// writes, and under ranges shaders write.
    UpdateFreeUs,
    UpdateBusyUs,
    /// Refresh batches done, and title writes that made the submission's
    /// copies due again.
    BatchFlushes,
    BatchInvalidations,
    /// Refreshes skipped (nothing changed in the range), refreshes run, and
    /// their time in nanoseconds (most take under a microsecond).
    CleanSkips,
    Updates,
    UpdateNs,
    /// Ranges shaders write that updates looked through, and the time
    /// finding those over the range.
    PendingScanned,
    PendingUs,
    /// Pages given to copy by change checks (a page counts each time), those
    /// of them covered only in part by the range asked (they stay changed,
    /// so the next check copies them again), whole ones that could not be
    /// guarded (they stay changed too), and the distinct pages of each frame.
    TakenPages,
    PartPages,
    UnguardedPages,
    DistinctTakenPages,
}
const COUNTERS: usize = 96;
static COUNT: [AtomicU64; COUNTERS] = [const { AtomicU64::new(0) }; COUNTERS];

pub(crate) fn add(c: C, n: u64) {
    COUNT[c as usize].fetch_add(n, Relaxed);
}

/// Heap slots made (again) by view kind (`View::kind`): count, microseconds.
static MADE: [[AtomicU64; 2]; 8] = [const { [const { AtomicU64::new(0) }; 2] }; 8];

/// A heap slot of view kind `kind` was made in `us`.
pub(crate) fn made(kind: usize, us: u64) {
    if let Some([n, t]) = MADE.get(kind) {
        n.fetch_add(1, Relaxed);
        t.fetch_add(us, Relaxed);
    }
}

/// Parts of making placed buffer views (kind 4) and buffer views at
/// large, nanoseconds: making the descriptor, the heap's bindless record,
/// the slot's held resource; `shadow::for_view` (all buffer views), and
/// copies of single views made (count, time).
pub(crate) const PART_MAKE: usize = 0;
pub(crate) const PART_VIEWED: usize = 1;
pub(crate) const PART_KEEP: usize = 2;
pub(crate) const PART_FOR_VIEW: usize = 3;
pub(crate) const PART_EXACT_N: usize = 4;
pub(crate) const PART_EXACT: usize = 5;
static PARTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

pub(crate) fn part(i: usize, n: u64) {
    PARTS[i].fetch_add(n, Relaxed);
}

/// How copies run: 0 guarded (the default), 1 plain, 2 not at all.
pub(crate) fn mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_COPY_MODE").as_deref() {
        Ok("plain") => 1,
        Ok("none") => 2,
        _ => 0,
    })
}

/// Per source range (start, length): copies, bytes, microseconds, memory type.
type Ranges = HashMap<(usize, usize), (u64, u64, u64, u32)>;
static RANGES: Mutex<Option<Ranges>> = Mutex::new(None);

/// A copy of `len` bytes from `src` (memory type `kind`) took `us`.
pub(crate) fn range(src: usize, len: usize, us: u64, kind: u32) {
    let mut map = RANGES.lock().unwrap_or_else(|e| e.into_inner());
    let e = map
        .get_or_insert_with(HashMap::new)
        .entry((src, len))
        .or_insert((0, 0, 0, kind));
    e.0 += 1;
    e.1 += len as u64;
    e.2 += us;
}

/// Snapshot bytes per host copy (its title start and length).
static SNAPSHOTS: Mutex<Option<HashMap<(u64, u64), u64>>> = Mutex::new(None);

/// A snapshot of `bytes` of the copy of `[start, start + len)`.
pub(crate) fn snapshot(start: u64, len: u64, bytes: u64) {
    *SNAPSHOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .entry((start, len))
        .or_default() += bytes;
}

/// Our process's working set, bytes.
fn working_set() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: our own process and a sized structure.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    c.WorkingSetSize as u64
}

/// Our process's hard page faults so far (read from disk or the page
/// file), from the system's process list: `HardFaultCount` at +0x10 of
/// each entry, the process id at +0x50.
fn hard_faults() -> u64 {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(class: u32, info: *mut u8, len: u32, ret: *mut u32) -> i32;
    }
    const PROCESSES: u32 = 5;
    let id = std::process::id() as usize;
    let mut buf = vec![0u8; 4 << 20];
    let mut ret = 0;
    // SAFETY: a buffer of that length.
    if unsafe { NtQuerySystemInformation(PROCESSES, buf.as_mut_ptr(), buf.len() as u32, &mut ret) } < 0 {
        return 0;
    }
    let mut at = 0usize;
    loop {
        let entry = &buf[at..];
        let next = u32::from_le_bytes(entry[0..4].try_into().unwrap_or_default()) as usize;
        let pid = usize::from_le_bytes(entry[0x50..0x58].try_into().unwrap_or_default());
        if pid == id {
            return u64::from(u32::from_le_bytes(entry[0x10..0x14].try_into().unwrap_or_default()));
        }
        if next == 0 || at + next + 0x58 > buf.len() {
            return 0;
        }
        at += next;
    }
}

pub(crate) fn page_faults() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: our own process and a sized structure.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    u64::from(c.PageFaultCount)
}

/// Logs the counters per frame over `frames` frames, and the ranges that
/// took most bytes.
pub(crate) fn log(frames: f64) {
    static FAULTS: AtomicU64 = AtomicU64::new(0);
    let faults = page_faults();
    let faults = faults - FAULTS.swap(faults, Relaxed).min(faults);
    let c: Vec<f64> = COUNT
        .iter()
        .map(|a| a.swap(0, Relaxed) as f64 / frames)
        .collect();
    let mb = |v: f64| v / 1e6;
    let ms = |v: f64| v / 1e3;
    tracing::info!(
        "copies per frame: {:.1} submissions; {:.0} title copies {:.1} MB; {:.0} refreshes {:.1} MB, \
         {:.0} skipped {:.1} MB; {:.0} runs {:.1} ms; {:.0} queries {:.2} ms, moves {:.2} ms, \
         {:.2} MB unmapped; {:.0} page faults; {:.0} watches {:.2} ms, {:.1} MB checked, {:.1} MB \
         unchanged; {:.1} fallbacks; {:.0} guard faults; textures {:.1} MB watched and \
         changed, {:.1} MB hashed, {:.1} MB of them changed; watch time: kernel {:.1} ms, view queries {:.1} ms, protecting {:.1} ms; copied whole (not tracked) {:.1} MB in {:.1} ms; snapshots {:.1} MB in {:.1} ms; page faults {:.0} preparing, {:.0} copying back; {:.0} tracked pages marked through another mapping; \
         guards: {:.2} ms in the handler, {:.0} protect calls for {:.0} pages, {:.0} pages opened; \
         {:.0} tracked pages written ({:.0} of them the frame before too); change checks: {:.1} ms \
         asking for other mappings, {:.1} ms waiting for the guard lock; preparing: {:.1} ms in \
         refresh batches, steps (each with the batch it flushes): refresh {:.1} ms, copy {:.1} ms, \
         work {:.1} ms, GPU writes {:.1} ms, textures {:.1} ms, heaps {:.1} ms, title memory {:.1} ms; \
         heap texture uploads {:.1} ms, state preludes {:.1} ms, heaps synced {:.1} ms, textures touched {:.1} ms, last batch {:.1} ms, residency {:.1} ms, mark reads {:.1} ms, in all {:.1} ms; heap slots synced {:.0}, \
         made {:.0}; refreshes free {:.1} ms, busy {:.1} ms; copying back compared {:.1} MB          in {:.1} ms; marked pages: {:.0} compared, {:.1} MB written back in {:.1} ms, {:.0} kept the title's, {:.0} saved; working set {:.0} MB, \
         {:.0} hard faults",
        c[C::Submissions as usize],
        c[C::Copies as usize],
        mb(c[C::CopyBytes as usize]),
        c[C::Refreshes as usize],
        mb(c[C::RefreshBytes as usize]),
        c[C::Skipped as usize],
        mb(c[C::SkippedBytes as usize]),
        c[C::Runs as usize],
        ms(c[C::RunUs as usize]),
        c[C::Queries as usize],
        ms(c[C::QueryUs as usize]),
        ms(c[C::MoveUs as usize]),
        mb(c[C::Unmapped as usize]),
        faults as f64 / frames,
        c[C::Watches as usize],
        ms(c[C::WatchUs as usize]),
        mb(c[C::Checked as usize]),
        mb(c[C::Unchanged as usize]),
        c[C::Fallbacks as usize],
        c[C::GuardFaults as usize],
        mb(c[C::TexWatched as usize]),
        mb(c[C::TexHashed as usize]),
        mb(c[C::TexChanged as usize]),
        ms(c[C::KernelUs as usize]),
        ms(c[C::ViewQueryUs as usize]),
        ms(c[C::ProtectUs as usize]),
        mb(c[C::WholeBytes as usize]),
        ms(c[C::WholeUs as usize]),
        mb(c[C::SnapshotBytes as usize]),
        ms(c[C::SnapshotUs as usize]),
        c[C::FaultsPrep as usize],
        c[C::FaultsBack as usize],
        c[C::AliasMarks as usize],
        ms(c[C::HandlerUs as usize]),
        c[C::ProtectCalls as usize],
        c[C::ProtectPages as usize],
        c[C::OpenPages as usize],
        c[C::WrittenPages as usize],
        c[C::WrittenAgain as usize],
        ms(c[C::AliasUs as usize]),
        ms(c[C::GuardWaitUs as usize]),
        ms(c[C::FlushUs as usize]),
        ms(c[C::StepRefreshUs as usize]),
        ms(c[C::StepCopyUs as usize]),
        ms(c[C::StepWorkUs as usize]),
        ms(c[C::StepGpuWriteUs as usize]),
        ms(c[C::StepTextureUs as usize]),
        ms(c[C::StepHeapUs as usize]),
        ms(c[C::StepMemoryUs as usize]),
        ms(c[C::UploadsUs as usize]),
        ms(c[C::PreludeUs as usize]),
        ms(c[C::SyncHeapsUs as usize]),
        ms(c[C::TouchUs as usize]),
        ms(c[C::LastFlushUs as usize]),
        ms(c[C::ResidentUs as usize]),
        ms(c[C::MarksUs as usize]),
        ms(c[C::PrepareUs as usize]),
        c[C::SyncSlots as usize],
        c[C::SyncMade as usize],
        ms(c[C::UpdateFreeUs as usize]),
        ms(c[C::UpdateBusyUs as usize]),
        mb(c[C::CompareBytes as usize]),
        ms(c[C::CompareUs as usize]),
        c[C::MarkedPages as usize],
        mb(c[C::MarkedBytes as usize]),
        ms(c[C::MarkedUs as usize]),
        c[C::MarkedKept as usize],
        c[C::SavedPages as usize],
        working_set() as f64 / 1e6,
        {
            static HARD: AtomicU64 = AtomicU64::new(0);
            let hard = hard_faults();
            (hard - HARD.swap(hard, Relaxed).min(hard)) as f64 / frames
        },
    );
    tracing::info!(
        "change checks per frame: {:.0} pages given to copy, {:.0} distinct in their frame; stay changed: {:.0} covered in part, {:.0} not guarded",
        c[C::TakenPages as usize],
        c[C::DistinctTakenPages as usize],
        c[C::PartPages as usize],
        c[C::UnguardedPages as usize],
    );
    tracing::info!(
        "copy-back per frame: {:.1} MB in pieces the GPU changed; compare {:.1} ms, title reads \
         {:.1} ms, merges {:.1} ms, title writes {:.1} ms, watch {:.1} ms",
        mb(c[C::BackDiffBytes as usize]),
        ms(c[C::BackCompareUs as usize]),
        ms(c[C::BackReadUs as usize]),
        ms(c[C::BackMergeUs as usize]),
        ms(c[C::BackWriteUs as usize]),
        ms(c[C::BackWatchUs as usize]),
    );
    let kinds = ["constant", "shader", "unordered", "sampler", "placed buffer", "placed texture", "placed texture left for draws", "placed unordered"];
    let made: Vec<String> = MADE
        .iter()
        .zip(kinds)
        .map(|([n, t], kind)| {
            let (n, t) = (n.swap(0, Relaxed) as f64 / frames, t.swap(0, Relaxed) as f64 / 1e3 / frames);
            format!("{kind} {n:.0} in {t:.1} ms")
        })
        .collect();
    tracing::info!("  heap slots made per frame by kind: {}", made.join(", "));
    let [mk, viewed, keep, for_view, exact_n, exact] = [0, 1, 2, 3, 4, 5].map(|i| PARTS[i].swap(0, Relaxed) as f64 / frames);
    tracing::info!(
        "  placed buffer parts per frame: make {:.1} ms, bindless {:.1} ms, keep {:.1} ms; for_view (all) {:.1} ms, view copies made {exact_n:.0} in {:.1} ms",
        mk / 1e6,
        viewed / 1e6,
        keep / 1e6,
        for_view / 1e6,
        exact / 1e6
    );
    let held: Vec<String> = crate::watch::held_times()
        .iter()
        .map(|(line, total, longest)| format!("watch.rs:{line} {:.1} ms (longest {longest:.1})", total / frames))
        .collect();
    tracing::info!("  guard lock held per frame: {}", held.join(", "));
    let mut snapshots: Vec<((u64, u64), u64)> =
        SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner()).take().into_iter().flatten().collect();
    snapshots.sort_by_key(|&(_, bytes)| std::cmp::Reverse(bytes));
    let snapshots: Vec<String> = snapshots
        .iter()
        .take(6)
        .map(|((start, len), bytes)| format!("{start:#x}+{len:#x} {:.1} MB", *bytes as f64 / 1e6 / frames))
        .collect();
    tracing::info!("  snapshots per frame by copy: {}", snapshots.join(", "));
    tracing::info!(
        "refresh batches per frame: {:.0} flushes, {:.0} invalidations; {:.0} refreshes of unchanged          ranges skipped, {:.0} run in {:.1} ms; {:.0} shader-write ranges scanned in {:.1} ms",
        c[C::BatchFlushes as usize],
        c[C::BatchInvalidations as usize],
        c[C::CleanSkips as usize],
        c[C::Updates as usize],
        c[C::UpdateNs as usize] / 1e6,
        c[C::PendingScanned as usize],
        ms(c[C::PendingUs as usize]),
    );
    let ranges = RANGES.lock().unwrap_or_else(|e| e.into_inner()).take();
    let mut ranges: Vec<_> = ranges.into_iter().flatten().collect();
    ranges.sort_by_key(|&(_, (_, bytes, _, _))| std::cmp::Reverse(bytes));
    for ((src, len), (n, bytes, us, _)) in ranges.into_iter().take(10) {
        use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        unsafe { VirtualQuery(src as *const _, &mut info, size_of_val(&info)) };
        let kind = info.Type;
        tracing::info!(
            "  copied from {src:#x}+{len:#x} (memory {kind:#x}): {:.1}x/frame, {:.2} MB/frame, {:.2} ms/frame",
            n as f64 / frames,
            bytes as f64 / 1e6 / frames,
            us as f64 / 1e3 / frames
        );
    }
}

/// Runs its closure when dropped (timing a function with several returns).
pub(crate) struct OnDrop<F: FnMut()>(pub(crate) F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}
