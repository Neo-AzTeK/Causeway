//! Scratch memory of emulated mesh pipelines (`pipeline::host`, module
//! notes): one buffer the mesh programs made as compute write their
//! vertices, then their triangles, to; and a table of group numbers, whose
//! entry `n` a compute pass that starts at the title's group `n` reads as
//! its first group's (`victory_shader::emit`, "Mesh shaders as compute").
//!
//! A mesh dispatch runs in chunks of as many groups as the scratch holds:
//! per chunk the compute pass, a barrier, the draw, a barrier back to the
//! common state. So every chunk starts at the scratch's start, and a list
//! leaves the scratch as it found it.
//!
//! shortcut: one compute-to-graphics drain per chunk (about one per mesh
//! draw); batch the compute passes of consecutive mesh draws if frames show
//! the drains. 64 MiB of video memory, made on the first emulated draw.

#[cfg(windows)]
use windows::Win32::Graphics::Direct3D12::*;
#[cfg(windows)]
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

/// Bytes of scratch.
pub(crate) const SCRATCH: u64 = 64 << 20;
/// Entries of the group-number table: one per group a mesh dispatch may
/// have (the console's and the indirect clamp's 65,535) and one more.
pub(crate) const BASES: u32 = 1 << 16;

/// The scratch buffer (default heap, unordered access, common state
/// between uses) and the group-number table (upload heap).
#[cfg(windows)]
pub(crate) struct Scratch {
    pub(crate) buffer: ID3D12Resource,
    pub(crate) bases: ID3D12Resource,
    /// The dispatch and draw records of indirect mesh dispatches
    /// ([`convert`]; default heap, common state between uses).
    pub(crate) commands: ID3D12Resource,
}

/// The scratch, made once; `None` if the host could not make it.
#[cfg(windows)]
pub(crate) fn scratch() -> Option<&'static Scratch> {
    static SCRATCH_BUFFERS: std::sync::OnceLock<Option<Scratch>> = std::sync::OnceLock::new();
    SCRATCH_BUFFERS
        .get_or_init(|| {
            let made = make();
            if let Err(e) = &made {
                tracing::warn!(error = %e, "no mesh scratch: emulated mesh draws are dropped");
            }
            made.ok()
        })
        .as_ref()
}

#[cfg(windows)]
fn make() -> windows::core::Result<Scratch> {
    let device = &crate::host::host().ok_or_else(windows::core::Error::empty)?.device;
    let buffer = |heap: D3D12_HEAP_TYPE, bytes: u64, flags, state| -> windows::core::Result<ID3D12Resource> {
        let props = D3D12_HEAP_PROPERTIES {
            Type: heap,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: bytes,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        };
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: plain creation of a committed buffer.
        unsafe { device.CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut made)? };
        made.ok_or_else(windows::core::Error::empty)
    };
    let scratch = buffer(
        D3D12_HEAP_TYPE_DEFAULT,
        SCRATCH,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_COMMON,
    )?;
    let bases = buffer(
        D3D12_HEAP_TYPE_UPLOAD,
        u64::from(BASES) * 4,
        D3D12_RESOURCE_FLAG_NONE,
        D3D12_RESOURCE_STATE_GENERIC_READ,
    )?;
    let commands = buffer(
        D3D12_HEAP_TYPE_DEFAULT,
        u64::from(DRAWS_AT + RECORDS * DRAW_RECORD),
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_COMMON,
    )?;
    let mut p = std::ptr::null_mut();
    // SAFETY: an upload buffer of BASES dwords, mapped whole and written
    // once before any list reads it.
    unsafe {
        bases.Map(0, None, Some(&mut p))?;
        let table = std::slice::from_raw_parts_mut(p.cast::<u32>(), BASES as usize);
        for (n, entry) in table.iter_mut().enumerate() {
            *entry = n as u32;
        }
        bases.Unmap(0, None);
    }
    crate::host::name(&scratch, || "mesh scratch".to_owned());
    Ok(Scratch { buffer: scratch, bases, commands })
}

/// Records of one indirect mesh dispatch the emulation runs; more run
/// empty.
pub(crate) const RECORDS: u32 = 4096;
/// Bytes of one dispatch record in the commands buffer: the vertices' and
/// triangles' UAVs, the first group's SRV (root views), the group counts.
pub(crate) const DISPATCH_RECORD: u32 = 36;
/// Bytes of one draw record: the vertices' SRV, the index buffer view, the
/// indexed draw's arguments.
pub(crate) const DRAW_RECORD: u32 = 44;
/// Where the draw records start in the commands buffer.
pub(crate) const DRAWS_AT: u32 = RECORDS * DISPATCH_RECORD;

/// Turns clamped mesh dispatch records into the compute pass's dispatch
/// records and the draw pass's draw records. One lane walks the records in
/// order: record `r`'s groups take the scratch from where record `r - 1`'s
/// end (its vertices, then its triangles); a record whose groups do not
/// fit, or past the count, runs empty.
#[cfg(windows)]
const CONVERT: &str = r#"
#define RS "RootConstants(num32BitConstants=12, b0), UAV(u0), UAV(u1), UAV(u2)"
cbuffer Call : register(b0) {
    uint stride; uint records; uint counted; uint vertex_bytes;
    uint index_bytes; uint capacity; uint primitives; uint scratch_lo;
    uint scratch_hi; uint bases_lo; uint bases_hi; uint draws_at;
};
RWByteAddressBuffer args : register(u0);
RWByteAddressBuffer count : register(u1);
RWByteAddressBuffer commands : register(u2);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    uint n = records;
    if (counted != 0) {
        n = min(count.Load(0), records);
    }
    uint used = 0;
    for (uint r = 0; r < records; r++) {
        uint3 g = args.Load3(r * stride);
        uint x = (r < n && g.y != 0 && g.z != 0) ? g.x : 0;
        if (x > capacity - used) {
            x = 0;
        }
        uint vertices = scratch_lo + used * (vertex_bytes + index_bytes);
        uint vertices_hi = scratch_hi + (vertices < scratch_lo ? 1 : 0);
        uint indices = vertices + x * vertex_bytes;
        uint indices_hi = vertices_hi + (indices < vertices ? 1 : 0);
        uint at = r * 36;
        commands.Store4(at, uint4(vertices, vertices_hi, indices, indices_hi));
        commands.Store4(at + 16, uint4(bases_lo, bases_hi, x, 1));
        commands.Store(at + 32, 1);
        at = draws_at + r * 44;
        commands.Store4(at, uint4(vertices, vertices_hi, indices, indices_hi));
        // R32_UINT is DXGI format 42.
        commands.Store4(at + 16, uint4(x * index_bytes, 42, x * primitives * 3, 1));
        commands.Store3(at + 32, uint3(0, 0, 0));
        used += x;
    }
}
"#;

#[cfg(windows)]
struct Converter {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

// SAFETY: D3D12 objects are free-threaded.
#[cfg(windows)]
unsafe impl Send for Converter {}
// SAFETY: as above.
#[cfg(windows)]
unsafe impl Sync for Converter {}

#[cfg(windows)]
fn converter() -> Option<&'static Converter> {
    static CONVERTER: std::sync::OnceLock<Option<Converter>> = std::sync::OnceLock::new();
    CONVERTER
        .get_or_init(|| match make_converter() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no mesh record converter: emulated indirect mesh dispatches dropped");
                None
            }
        })
        .as_ref()
}

#[cfg(windows)]
fn make_converter() -> windows::core::Result<Converter> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::core::s;
    let device = &crate::host::host().ok_or_else(windows::core::Error::empty)?.device;
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            CONVERT.as_ptr().cast(),
            CONVERT.len(),
            s!("mesh records"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe {
                std::slice::from_raw_parts(errors.GetBufferPointer().cast::<u8>(), errors.GetBufferSize())
            };
            tracing::warn!("mesh record converter: {}", String::from_utf8_lossy(text));
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
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: bytes.as_ptr().cast(),
                BytecodeLength: bytes.len(),
            },
            ..Default::default()
        })?;
        Ok(Converter { root, pso })
    }
}

/// Records, in `list`, the conversion of `records` clamped mesh dispatch
/// records `stride` bytes apart at host address `args` (unordered access),
/// their count at `count` if given (likewise), into the commands buffer,
/// left as indirect arguments. `layout`: vertex bytes and triangles per
/// group, groups the scratch holds. Replaces the list's compute root
/// signature and pipeline. False without the converter.
#[cfg(windows)]
pub(crate) fn convert(
    list: &crate::host::HostList,
    scratch: &Scratch,
    (args, count): (u64, Option<u64>),
    (stride, records): (u32, u32),
    (vertex_bytes, primitives, capacity): (u32, u32, u32),
) -> bool {
    let Some(c) = converter() else {
        return false;
    };
    let l = &list.list;
    // SAFETY: live buffers.
    let (base, bases) = unsafe { (scratch.buffer.GetGPUVirtualAddress(), scratch.bases.GetGPUVirtualAddress()) };
    let constants = [
        stride,
        records,
        u32::from(count.is_some()),
        vertex_bytes,
        primitives * 12,
        capacity,
        primitives,
        base as u32,
        (base >> 32) as u32,
        bases as u32,
        (bases >> 32) as u32,
        DRAWS_AT,
    ];
    // SAFETY: an open list; our root signature, pipeline and buffers, and
    // title buffers' host copies the caller's list holds.
    unsafe {
        l.SetComputeRootSignature(&c.root);
        l.SetPipelineState(&c.pso);
        l.SetComputeRoot32BitConstants(0, constants.len() as u32, constants.as_ptr().cast(), 0);
        l.SetComputeRootUnorderedAccessView(1, args);
        l.SetComputeRootUnorderedAccessView(2, count.unwrap_or(args));
        l.SetComputeRootUnorderedAccessView(3, scratch.commands.GetGPUVirtualAddress());
        l.ResourceBarrier(&[crate::host::transition(
            &scratch.commands,
            D3D12_RESOURCE_STATE_COMMON,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )]);
        l.Dispatch(1, 1, 1);
        l.ResourceBarrier(&[crate::host::transition(
            &scratch.commands,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
        )]);
    }
    true
}

/// The command signatures of an emulated mesh pipeline's indirect
/// dispatches: the compute pass's (root signature `compute`, root
/// parameters of the vertices' and triangles' UAVs and the first group's
/// SRV) and the draw pass's (root signature `draw`, parameter of the
/// vertices' SRV), for the records [`convert`] writes.
#[cfg(windows)]
pub(crate) fn signatures(
    device: &ID3D12Device,
    (compute, [vertices, triangles, first]): (&ID3D12RootSignature, [u32; 3]),
    (draw, drawn): (&ID3D12RootSignature, u32),
) -> windows::core::Result<(ID3D12CommandSignature, ID3D12CommandSignature)> {
    let view = |kind, index| {
        let mut a = D3D12_INDIRECT_ARGUMENT_DESC {
            Type: kind,
            ..Default::default()
        };
        // Every view argument's description is its root parameter index.
        a.Anonymous.ShaderResourceView.RootParameterIndex = index;
        a
    };
    let plain = |kind| D3D12_INDIRECT_ARGUMENT_DESC {
        Type: kind,
        ..Default::default()
    };
    let make = |root: &ID3D12RootSignature, stride, arguments: &[D3D12_INDIRECT_ARGUMENT_DESC]| {
        let desc = D3D12_COMMAND_SIGNATURE_DESC {
            ByteStride: stride,
            NumArgumentDescs: arguments.len() as u32,
            pArgumentDescs: arguments.as_ptr(),
            NodeMask: 0,
        };
        let mut signature: Option<ID3D12CommandSignature> = None;
        // SAFETY: the description points at live arguments.
        unsafe { device.CreateCommandSignature(&desc, root, &mut signature)? };
        signature.ok_or_else(windows::core::Error::empty)
    };
    let dispatch = make(
        compute,
        DISPATCH_RECORD,
        &[
            view(D3D12_INDIRECT_ARGUMENT_TYPE_UNORDERED_ACCESS_VIEW, vertices),
            view(D3D12_INDIRECT_ARGUMENT_TYPE_UNORDERED_ACCESS_VIEW, triangles),
            view(D3D12_INDIRECT_ARGUMENT_TYPE_SHADER_RESOURCE_VIEW, first),
            plain(D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH),
        ],
    )?;
    let drawing = make(
        draw,
        DRAW_RECORD,
        &[
            view(D3D12_INDIRECT_ARGUMENT_TYPE_SHADER_RESOURCE_VIEW, drawn),
            plain(D3D12_INDIRECT_ARGUMENT_TYPE_INDEX_BUFFER_VIEW),
            plain(D3D12_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED),
        ],
    )?;
    Ok((dispatch, drawing))
}

/// Groups of a mesh program the scratch holds at once (vertex bytes and
/// slots per group as its layout says); 0 if not one.
pub(crate) fn groups_per_chunk(stride: u32, max_vertices: u32, max_primitives: u32) -> u32 {
    let per_group = u64::from(max_vertices) * u64::from(stride) + u64::from(max_primitives) * 12;
    match per_group {
        0 => 0,
        n => (SCRATCH / n).min(u64::from(BASES - 1)) as u32,
    }
}

/// A dispatch of `groups` groups as chunks of at most `per_chunk`: (first
/// group, groups).
pub(crate) fn chunks(groups: u32, per_chunk: u32) -> impl Iterator<Item = (u32, u32)> {
    let step = per_chunk.max(1);
    (0..groups.div_ceil(step)).map(move |k| (k * step, step.min(groups - k * step)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_the_dispatch_in_order() {
        assert_eq!(chunks(10, 4).collect::<Vec<_>>(), [(0, 4), (4, 4), (8, 2)]);
        assert_eq!(chunks(8, 4).collect::<Vec<_>>(), [(0, 4), (4, 4)]);
        assert_eq!(chunks(3, 100).collect::<Vec<_>>(), [(0, 3)]);
        assert_eq!(chunks(0, 4).count(), 0);
        // The largest dispatch, in chunks the table numbers.
        let all: Vec<_> = chunks(65_535, 4_000).collect();
        assert_eq!(all.iter().map(|c| c.1).sum::<u32>(), 65_535);
        assert!(all.iter().all(|&(first, _)| first < BASES));
    }

    #[test]
    fn chunks_fit_the_scratch() {
        // The title's usual program: 128 vertices of 7 elements, 128
        // triangles: 15,872 bytes a group.
        let n = groups_per_chunk(7 * 16, 128, 128);
        assert_eq!(n, 4_228);
        assert!(u64::from(n) * (128 * 112 + 128 * 12) <= SCRATCH);
        // The host's largest: 256 vertices of 32 elements.
        assert!(groups_per_chunk(32 * 16, 256, 256) >= 1);
        // Tiny groups stop at the table's last entry.
        assert_eq!(groups_per_chunk(16, 1, 1), BASES - 1);
        assert_eq!(groups_per_chunk(0, 0, 0), 0);
    }
}
