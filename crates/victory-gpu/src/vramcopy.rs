//! Probe (diagnostics, `VICTORY_PROBE_VRAM_COPIES=1`): host copies of title
//! buffers in video memory, to test whether shaders reading them from system
//! memory over the bus is what the GPU's time goes to (`docs/frame-cost.md`,
//! H6.1; `docs/memory-design.md`, step 2).
//!
//! A copy made while the budget has room ([`crate::vram::room`]) gets a
//! buffer in video memory that every view and address of it uses. Its system
//! memory buffer stays the CPU's side: refreshes write there as before, and
//! each range they write is copied to video memory by one list that runs
//! first in the submission. What shaders write stays in video memory: copy-
//! back compares title memory with the system buffer, finds no change and
//! writes nothing back. So the title never sees GPU results, a page the title
//! and shaders both wrote gets the title's older bytes, and the picture may
//! be wrong: this measures GPU time, it is not a fix. Copies of single views
//! (`shadow::exact`) stay in system memory.

use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

/// Whether the probe is on (read once).
pub(crate) fn wanted() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_PROBE_VRAM_COPIES").is_ok_and(|v| v == "1"))
}

/// A buffer of `size` bytes in video memory that shaders read and write,
/// for a copy whose system buffer is as large; none when the probe is off,
/// the budget has no room or the host refuses.
pub(crate) fn twin(size: u64) -> Option<ID3D12Resource> {
    if !wanted() || !crate::vram::room() {
        return None;
    }
    let host = crate::host::host()?;
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    // SAFETY: plain creation; committed buffers start zeroed.
    let made = unsafe {
        host.device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut buffer)
    };
    match made {
        Ok(()) => {
            let mut n = TWINS.lock().unwrap_or_else(|e| e.into_inner());
            n.0 += 1;
            n.1 += size;
            if n.0.is_power_of_two() {
                tracing::info!("video-memory copies (probe): {} made, {} MB", n.0, n.1 >> 20);
            }
            buffer
        }
        Err(e) => {
            tracing::warn!(error = %e, size, "no video-memory copy (probe): stays in system memory");
            None
        }
    }
}

/// Video-memory copies made so far, and their bytes.
static TWINS: Mutex<(u64, u64)> = Mutex::new((0, 0));

/// One range to bring to video memory: (destination, source, offset, bytes).
type Range = (ID3D12Resource, ID3D12Resource, u64, u64);

/// Ranges written to system buffers since the last submission.
static DUE: Mutex<Vec<Range>> = Mutex::new(Vec::new());

/// A refresh wrote `n` bytes at `offset` of `from` (system memory), whose
/// copy shaders use in `to` (video memory).
pub(crate) fn copied(to: &ID3D12Resource, from: &ID3D12Resource, offset: u64, n: u64) {
    if n > 0 {
        DUE.lock().unwrap_or_else(|e| e.into_inner()).push((to.clone(), from.clone(), offset, n));
    }
}

/// The list and allocator the copies are recorded in, and the ranges of the
/// last recording, held until the next (one submission runs at a time, and
/// is waited for before the next is prepared: their buffers live while it
/// may run).
struct Recorder {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    running: Vec<Range>,
}

// SAFETY: D3D12 objects are free-threaded; used under the mutex only.
unsafe impl Send for Recorder {}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

/// At submission, after every refresh: a closed list that copies the ranges
/// due to video memory, to run before the title's lists; none when nothing
/// is due.
pub(crate) fn list(device: &ID3D12Device) -> Option<ID3D12CommandList> {
    let due = std::mem::take(&mut *DUE.lock().unwrap_or_else(|e| e.into_inner()));
    let mut recorder = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    if due.is_empty() {
        if let Some(r) = recorder.as_mut() {
            r.running.clear();
        }
        return None;
    }
    if recorder.is_none() {
        // SAFETY: plain creation; the list is closed until recorded.
        let made = unsafe {
            device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT).and_then(|allocator| {
                let list: ID3D12GraphicsCommandList =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
                list.Close()?;
                Ok(Recorder { allocator, list, running: Vec::new() })
            })
        };
        match made {
            Ok(r) => *recorder = Some(r),
            Err(e) => {
                tracing::warn!(error = %e, "no list for video-memory copies (probe)");
                return None;
            }
        }
    }
    let r = recorder.as_mut()?;
    let bytes: u64 = due.iter().map(|d| d.3).sum();
    // SAFETY: the last submission that used the list has run (each is
    // waited for); ranges are inside both buffers (cut to the copy).
    let recorded = unsafe {
        r.allocator.Reset().and_then(|()| r.list.Reset(&r.allocator, None)).and_then(|()| {
            for (to, from, offset, n) in &due {
                r.list.CopyBufferRegion(to, *offset, from, *offset, *n);
            }
            r.list.Close()
        })
    };
    r.running = due;
    COPIED.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    match recorded {
        Ok(()) => windows::core::Interface::cast(&r.list).ok(),
        Err(e) => {
            tracing::warn!(error = %e, "video-memory copies not recorded (probe)");
            None
        }
    }
}

/// Bytes copied to video memory since the last [`log`].
static COPIED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Logs the bytes copied to video memory per frame over `frames`.
pub(crate) fn log(frames: f64) {
    let bytes = COPIED.swap(0, std::sync::atomic::Ordering::Relaxed);
    if wanted() && frames > 0.0 {
        tracing::info!("video-memory copies (probe): {:.1} MB copied per frame", bytes as f64 / frames / 1e6);
    }
}
