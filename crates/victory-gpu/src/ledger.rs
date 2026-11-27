//! Memory by owner (`docs/memory-design.md`, step 0): what each part of
//! the process holds, kept by the parts themselves and logged with the
//! frame rate, with or without diagnostics. Figures that live elsewhere
//! (host textures, video memory) are logged there.

use std::sync::atomic::{AtomicI64, Ordering::Relaxed};

#[derive(Clone, Copy)]
pub enum Owner {
    /// Host copies of title buffers (system memory the GPU reads).
    BufferCopies,
    /// Snapshot memory kept for reuse.
    SnapshotPool,
    /// Title pages left without bytes by disk loads, now.
    LazyPages,
    /// Bytes of such pages filled since start (title accesses and writes).
    LazyFilled,
    /// Of those, pages the title's CPU touched (faults).
    LazyFaults,
    /// Bytes read or decoded from load sources since start.
    SourceRead,
    /// Decoded loads cached.
    DecodeCache,
    /// Bytes of disk loads written into title memory since start (not left
    /// without bytes).
    LoadsWritten,
    /// The pipeline library's serialized size.
    PipelineLibrary,
    /// Title physical-page sections, committed.
    TitleSections,
}

const N: usize = 10;
static BYTES: [AtomicI64; N] = [const { AtomicI64::new(0) }; N];

pub fn add(owner: Owner, n: i64) {
    BYTES[owner as usize].fetch_add(n, Relaxed);
}

pub fn set(owner: Owner, n: i64) {
    BYTES[owner as usize].store(n, Relaxed);
}

/// Bytes held by `owner` now.
pub fn bytes(owner: Owner) -> i64 {
    BYTES[owner as usize].load(Relaxed)
}

fn get(owner: Owner) -> f64 {
    BYTES[owner as usize].load(Relaxed) as f64
}

/// One log line: the process's figures, then each owner's.
pub(crate) fn line() -> String {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    // SAFETY: our own process and a sized structure.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    let mb = |v: f64| v / 1e6;
    // The host's commit (every process's, sections included) against its
    // limit: what a failed allocation ran into.
    let mut p = windows_sys::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION {
        cb: size_of::<windows_sys::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: a sized structure.
    unsafe { windows_sys::Win32::System::ProcessStatus::K32GetPerformanceInfo(&mut p, p.cb) };
    let page = p.PageSize as f64;
    format!(
        "memory: host commit {:.0} of {:.0} MB; private {:.0} MB, working set {:.0} MB; title sections {:.0} MB, buffer copies {:.0} MB, \
         snapshot pool {:.0} MB, pipeline library {:.0} MB, decode cache {:.0} MB; loads left without \
         bytes {:.0} MB now, filled {:.0} MB ({:.0} MB by title accesses), read from sources {:.0} MB; loads written {:.0} MB",
        mb(p.CommitTotal as f64 * page),
        mb(p.CommitLimit as f64 * page),
        mb(c.PagefileUsage as f64),
        mb(c.WorkingSetSize as f64),
        mb(get(Owner::TitleSections)),
        mb(get(Owner::BufferCopies)),
        mb(get(Owner::SnapshotPool)),
        mb(get(Owner::PipelineLibrary)),
        mb(get(Owner::DecodeCache)),
        mb(get(Owner::LazyPages)),
        mb(get(Owner::LazyFilled)),
        mb(get(Owner::LazyFaults)),
        mb(get(Owner::SourceRead)),
        mb(get(Owner::LoadsWritten)),
    )
}
