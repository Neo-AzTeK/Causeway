//! Detiling on the host GPU (`crate::texture`): the tiled bytes go
//! unchanged into an upload buffer, a compute shader gathers them into
//! linear rows in a buffer in video memory, and that buffer is copied into
//! the texture. Both buffers are the list's until it is reset, then go back
//! to a pool. Only the subresources whose tiled bytes may have changed are
//! converted.
//!
//! The shader does no tiling arithmetic: `victory_formats` gives, for each
//! subresource, a table of row and column offsets
//! (`Layout::tables`), which follow the tiled bytes in the upload buffer.
//!
//! Only root descriptors: no descriptor heap. Recorded only into the
//! runtime's own lists, so no title list's bound state changes.
//!
//! shortcut: elements under 4 bytes (the shader moves whole dwords) are
//! detiled on the CPU, and so is everything if the shader is not made.

use std::sync::Mutex;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host};
use crate::object::This;

const SOURCE: &str = r#"
#define SIGNATURE "RootConstants(num32BitConstants=8, b0), SRV(t0), UAV(u0)"
cbuffer Parameters : register(b0) {
    uint RowDwords;
    uint Height;
    uint ElementDwords;
    uint Out;
    uint Pitch;
    uint SlicePitch;
    uint Columns;
    uint Rows;
};
ByteAddressBuffer Tiled : register(t0);
RWByteAddressBuffer Linear : register(u0);

[RootSignature(SIGNATURE)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= RowDwords) return;
    uint column = Tiled.Load(Columns + (id.x / ElementDwords) * 4);
    uint row = Tiled.Load(Rows + (id.z * Height + id.y) * 4);
    Linear.Store(Out + id.z * SlicePitch + id.y * Pitch + id.x * 4,
        Tiled.Load(row + column + (id.x % ElementDwords) * 4));
}
"#;

struct Subresource {
    constants: [u32; 8],
    groups: [u32; 3],
    /// The tiled bytes it reads lie in this range (others may too).
    span: (usize, usize),
}

/// Buffers kept for the next conversion, up to this many bytes of each kind.
const IDLE: u64 = 256 << 20;
/// Larger buffers are made for one use.
const POOLED: u64 = 64 << 20;

struct Buffer {
    resource: ID3D12Resource,
    /// The mapping of an upload buffer; 0 for one in video memory.
    data: usize,
    size: u64,
}

// SAFETY: D3D12 objects are free-threaded; one list uses a buffer at a time.
unsafe impl Send for Buffer {}

/// Idle buffers: in video memory, and upload.
static POOL: Mutex<[Vec<Buffer>; 2]> = Mutex::new([Vec::new(), Vec::new()]);

/// A buffer a list holds; back to the pool when the list is reset.
pub(crate) struct Pooled(Option<Buffer>);

impl Pooled {
    fn get(host: &crate::host::Host, size: u64, upload: bool) -> Option<Pooled> {
        let size = if size > POOLED { size } else { size.max(1 << 16).next_power_of_two() };
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        let idle = &mut pool[usize::from(upload)];
        if let Some(i) = idle.iter().position(|b| b.size == size) {
            return Some(Pooled(Some(idle.swap_remove(i))));
        }
        drop(pool);
        let (resource, data) = if upload {
            let (resource, data) = host.upload_buffer(size)?;
            (resource, data as usize)
        } else {
            (host.unordered_buffer(size)?, 0)
        };
        Some(Pooled(Some(Buffer { resource, data, size })))
    }

    fn buffer(&self) -> &Buffer {
        self.0.as_ref().expect("held until dropped")
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        let Some(buffer) = self.0.take() else {
            return;
        };
        if buffer.size > POOLED {
            return;
        }
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        let idle = &mut pool[usize::from(buffer.data != 0)];
        if idle.iter().map(|b| b.size).sum::<u64>() + buffer.size <= IDLE {
            idle.push(buffer);
        }
    }
}

/// What a texture's conversion needs, computed once.
pub(crate) struct Plan {
    /// Bytes of tiled memory; the tables follow them.
    tiled: usize,
    tables: Vec<u32>,
    subresources: Vec<Subresource>,
    /// Bytes of the linear buffer.
    linear: u64,
}

impl Plan {
    /// None: this texture is detiled on the CPU.
    pub(crate) fn new(
        layout: &victory_formats::texture::Layout,
        footprints: &[D3D12_PLACED_SUBRESOURCE_FOOTPRINT],
        rows: &[u32],
        mips: u32,
        linear: u64,
    ) -> Option<Plan> {
        let element = layout.desc.element_bytes;
        if element < 4 || !element.is_multiple_of(4) {
            return None;
        }
        converter()?;
        let tiled = usize::try_from(layout.size).ok()?.next_multiple_of(4);
        let mut tables = Vec::new();
        let mut subresources = Vec::with_capacity(footprints.len());
        for (i, (footprint, &host_rows)) in footprints.iter().zip(rows).enumerate() {
            let (mip, slice) = (i as u32 % mips, i as u32 / mips);
            let t = layout.tables(mip, slice).ok()?;
            let f = footprint.Footprint;
            let row_bytes = u64::from(t.width) * u64::from(element);
            let slice_pitch = u64::from(f.RowPitch) * u64::from(host_rows);
            let end = footprint.Offset
                + u64::from(t.depth - 1) * slice_pitch
                + u64::from(t.height - 1) * u64::from(f.RowPitch)
                + row_bytes;
            if t.height > host_rows || row_bytes > u64::from(f.RowPitch) || end > linear {
                return None;
            }
            let low = |v: &[u32]| v.iter().copied().min().unwrap_or(0) as usize;
            let high = |v: &[u32]| v.iter().copied().max().unwrap_or(0) as usize;
            let span = (
                low(&t.rows) + low(&t.columns),
                (high(&t.rows) + high(&t.columns) + element as usize).min(layout.size as usize),
            );
            let columns = u32::try_from(tiled + tables.len() * 4).ok()?;
            tables.extend_from_slice(&t.columns);
            let row_table = u32::try_from(tiled + tables.len() * 4).ok()?;
            tables.extend_from_slice(&t.rows);
            let dwords = t.width * (element / 4);
            subresources.push(Subresource {
                constants: [
                    dwords,
                    t.height,
                    element / 4,
                    u32::try_from(footprint.Offset).ok()?,
                    f.RowPitch,
                    u32::try_from(slice_pitch).ok()?,
                    columns,
                    row_table,
                ],
                groups: [dwords.div_ceil(64), t.height, t.depth],
                span,
            });
        }
        Some(Plan { tiled, tables, subresources, linear })
    }

    /// Records the conversion into `list`, one of the runtime's own, and
    /// the copy into `texture` (the host copy of `resource`), which is left
    /// in the copy destination state. Only what reads from the `dirty` byte
    /// ranges of the tiled memory (none: all of it). `fill(to, ranges)`
    /// writes those ranges of the tiled bytes at their offsets from `to`.
    /// False if the host gave no buffers.
    pub(crate) fn record(
        &self,
        list: &HostList,
        resource: This,
        texture: &ID3D12Resource,
        footprints: &[D3D12_PLACED_SUBRESOURCE_FOOTPRINT],
        dirty: Option<&[(usize, usize)]>,
        fill: impl FnOnce(*mut u8, &[(usize, usize)]),
    ) -> bool {
        const COPY_DEST: u32 = 0x400;
        let (Some(host), Some(converter)) = (host(), converter()) else {
            return false;
        };
        let chosen: Vec<usize> = (0..self.subresources.len())
            .filter(|&i| {
                let (a, b) = self.subresources[i].span;
                dirty.is_none_or(|d| d.iter().any(|&(from, to)| from < b && a < to))
            })
            .collect();
        if chosen.is_empty() {
            crate::list::ensure_state(list, resource, COPY_DEST);
            return true;
        }
        // The ranges to read: the chosen spans, merged.
        let mut ranges: Vec<(usize, usize)> = chosen.iter().map(|&i| self.subresources[i].span).collect();
        ranges.sort_unstable();
        ranges.dedup_by(|next, kept| {
            let joined = next.0 <= kept.1;
            if joined {
                kept.1 = kept.1.max(next.1);
            }
            joined
        });
        let size = (self.tiled + self.tables.len() * 4) as u64;
        let (Some(from), Some(to)) = (Pooled::get(host, size, true), Pooled::get(host, self.linear, false)) else {
            return false;
        };
        let input = from.buffer().resource.clone();
        let data = from.buffer().data as *mut u8;
        let output = to.buffer().resource.clone();
        list.keep_pooled(from);
        list.keep_pooled(to);
        fill(data, &ranges);
        // SAFETY: the upload buffer holds the tables after the tiled bytes,
        // at a multiple of 4.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.tables.as_ptr(),
                data.add(self.tiled).cast::<u32>(),
                self.tables.len(),
            );
        }
        let l = &list.list;
        // SAFETY: an open list of the runtime's; live buffers, kept by the
        // list; the constants and dispatches stay inside them (`new`).
        unsafe {
            l.SetComputeRootSignature(&converter.root);
            l.SetPipelineState(&converter.pso);
            l.SetComputeRootShaderResourceView(1, input.GetGPUVirtualAddress());
            l.SetComputeRootUnorderedAccessView(2, output.GetGPUVirtualAddress());
            for &i in &chosen {
                let s = &self.subresources[i];
                l.SetComputeRoot32BitConstants(0, 8, s.constants.as_ptr().cast(), 0);
                let [x, y, z] = s.groups;
                l.Dispatch(x, y, z);
            }
            l.ResourceBarrier(&[crate::host::transition(
                &output,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
        }
        crate::list::ensure_state(list, resource, COPY_DEST);
        for &i in &chosen {
            let footprint = &footprints[i];
            let to = D3D12_TEXTURE_COPY_LOCATION {
                // SAFETY: a borrowed pointer the location never releases.
                pResource: unsafe { std::mem::transmute_copy(texture) },
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: i as u32 },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                // SAFETY: as above.
                pResource: unsafe { std::mem::transmute_copy(&output) },
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: *footprint },
            };
            // SAFETY: an open list; both locations name live resources.
            unsafe { l.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
        }
        true
    }
}

struct Converter {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Converter {}
// SAFETY: as above.
unsafe impl Sync for Converter {}

fn converter() -> Option<&'static Converter> {
    static MADE: std::sync::OnceLock<Option<Converter>> = std::sync::OnceLock::new();
    MADE.get_or_init(|| match compile() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "textures are detiled on the CPU: no detiling shader");
            None
        }
    })
    .as_ref()
}

fn compile() -> windows::core::Result<Converter> {
    let device = &host().ok_or_else(windows::core::Error::empty)?.device;
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(SOURCE.as_ptr().cast(), SOURCE.len(), s!("detile"), None, None, s!("main"), s!("cs_5_1"), 0, 0, &mut code, Some(&mut errors))
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe { std::slice::from_raw_parts(errors.GetBufferPointer().cast::<u8>(), errors.GetBufferSize()) };
            tracing::warn!("detiling shader: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    let code = code.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: the blob's own bytes; the root signature is inside the shader.
    unsafe {
        let bytes = std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
            ..Default::default()
        })?;
        Ok(Converter { root, pso })
    }
}
