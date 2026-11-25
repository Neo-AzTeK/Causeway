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

/// What the passes of an emulated mesh pipeline record with: its scratch
/// layout and our root parameters of the scratch views.
#[cfg(windows)]
pub(crate) struct Layout {
    /// Bytes per vertex; vertex and primitive slots per group.
    pub(crate) stride: u32,
    pub(crate) max_vertices: u32,
    pub(crate) max_primitives: u32,
    /// The compute pass's vertices' and triangles' UAVs and first group's
    /// SRV.
    pub(crate) compute: [u32; 3],
    /// The draw pass's vertices' SRV.
    pub(crate) draw: u32,
}

#[cfg(windows)]
const READ: D3D12_RESOURCE_STATES =
    D3D12_RESOURCE_STATES(D3D12_RESOURCE_STATE_INDEX_BUFFER.0 | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0);

/// Records a mesh dispatch of `groups` groups in chunks of `per_chunk`:
/// per chunk `bind(true)` binds the compute pass (its title root
/// arguments), the chunk's vertices and triangles are made, `bind(false)`
/// binds the draw pass and they are drawn as an indexed triangle list. The
/// list's topology is a triangle list after. False if a pass could not be
/// bound (earlier chunks were drawn).
#[cfg(windows)]
pub(crate) fn run_direct(
    list: &crate::host::HostList,
    scratch: &Scratch,
    layout: &Layout,
    (groups, per_chunk): (u32, u32),
    mut bind: impl FnMut(bool) -> bool,
) -> bool {
    let l = &list.list;
    // SAFETY: live buffers.
    let (base, bases) = unsafe { (scratch.buffer.GetGPUVirtualAddress(), scratch.bases.GetGPUVirtualAddress()) };
    let [vertices, triangles, first_group] = layout.compute;
    for (first, n) in chunks(groups, per_chunk) {
        if !bind(true) {
            return false;
        }
        let indices = base + u64::from(n) * u64::from(layout.max_vertices) * u64::from(layout.stride);
        // SAFETY: an open list with the compute pass bound; the scratch and
        // the table live for good.
        unsafe {
            l.SetComputeRootUnorderedAccessView(vertices, base);
            l.SetComputeRootUnorderedAccessView(triangles, indices);
            l.SetComputeRootShaderResourceView(first_group, bases + u64::from(first) * 4);
            l.ResourceBarrier(&[crate::host::transition(
                &scratch.buffer,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]);
            l.Dispatch(n, 1, 1);
            l.ResourceBarrier(&[crate::host::transition(&scratch.buffer, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, READ)]);
        }
        let ready = bind(false);
        // SAFETY: as above, with the draw pass bound when `ready`.
        unsafe {
            if ready {
                l.SetGraphicsRootShaderResourceView(layout.draw, base);
                l.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                    BufferLocation: indices,
                    SizeInBytes: n * layout.max_primitives * 12,
                    Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_UINT,
                }));
                l.IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                l.DrawIndexedInstanced(n * layout.max_primitives * 3, 1, 0, 0, 0);
            }
            l.ResourceBarrier(&[crate::host::transition(&scratch.buffer, READ, D3D12_RESOURCE_STATE_COMMON)]);
        }
        if !ready {
            return false;
        }
    }
    true
}

/// Records indirect mesh dispatches: `max` clamped records `stride` bytes
/// apart at host address `args` (unordered access), their count at
/// `count` if given, become dispatch and draw records ([`convert`]); then
/// `bind(true)` binds the compute pass, which runs the dispatches, and
/// `bind(false)` the draw pass, which runs the draws (`signatures`:
/// [`signatures`]). Replaces the list's compute pipeline; its topology is a
/// triangle list after. False if a step could not be recorded.
#[cfg(windows)]
pub(crate) fn run_indirect(
    list: &crate::host::HostList,
    scratch: &Scratch,
    layout: &Layout,
    (dispatches, draws): (&ID3D12CommandSignature, &ID3D12CommandSignature),
    (args, count): (u64, Option<u64>),
    (stride, max): (u32, u32),
    mut bind: impl FnMut(bool) -> bool,
) -> bool {
    let records = max.min(RECORDS);
    if max > records {
        static PAST: std::sync::Once = std::sync::Once::new();
        PAST.call_once(|| tracing::warn!(max, "indirect mesh dispatches past 4,096 records run empty"));
    }
    let capacity = groups_per_chunk(layout.stride, layout.max_vertices, layout.max_primitives);
    let per_group = (layout.stride * layout.max_vertices, layout.max_primitives, capacity);
    if !convert(list, scratch, (args, count), (stride, records), per_group) {
        return false;
    }
    let l = &list.list;
    let ran = bind(true) && {
        // SAFETY: an open list with the compute pass bound; our buffers
        // live for good.
        unsafe {
            l.ResourceBarrier(&[crate::host::transition(
                &scratch.buffer,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]);
            l.ExecuteIndirect(dispatches, records, &scratch.commands, 0, None, 0);
            l.ResourceBarrier(&[crate::host::transition(&scratch.buffer, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, READ)]);
        }
        let ready = bind(false);
        // SAFETY: as above, with the draw pass bound when `ready`.
        unsafe {
            if ready {
                l.IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                l.ExecuteIndirect(draws, records, &scratch.commands, u64::from(DRAWS_AT), None, 0);
            }
            l.ResourceBarrier(&[crate::host::transition(&scratch.buffer, READ, D3D12_RESOURCE_STATE_COMMON)]);
        }
        ready
    };
    // SAFETY: an open list and our live buffer.
    unsafe {
        l.ResourceBarrier(&[crate::host::transition(
            &scratch.commands,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
            D3D12_RESOURCE_STATE_COMMON,
        )]);
    }
    ran
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

/// The emulated passes on the host GPU, with pipelines made as
/// `pipeline::host` makes them: a one-wave mesh program whose lanes all
/// export the full-screen triangle (0, 1, 2) and, as vertex `lane`, the
/// position (4 (lane & 1) - 1, 2 (lane & 2) - 1, group number, 1); depth
/// clipping off, so every group draws. Runs on the adapter `host()` takes
/// (the hardware one; WARP with `VICTORY_D3D12_WARP=1` and diagnostics).
#[cfg(all(test, windows))]
mod gpu_tests {
    use super::*;
    use crate::host::{Host, HostList};
    use victory_shader::emit::{MESH_SPACE, Options};
    use victory_shader::native::{HwStage, MeshIo, NativeProgram, StageIo};
    use victory_shader::Container;
    use windows::Win32::Graphics::Dxgi::Common::*;

    const LANES: u32 = 32;
    /// The scratch is one: one test at a time.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const SIZE: u32 = 16;

    /// The console container (its output signature: SV_Position) and the
    /// mesh program made as compute.
    fn program() -> (Vec<u8>, victory_shader::emit::HostShader) {
        let words = [
            0x7E00_0505u32, // v_readfirstlane_b32 s0, v5
            0x7E0C_0C00,    // v_cvt_f32_u32 v6, s0
            0xD765_0001,    // v_mbcnt_lo_u32_b32 v1, -1, 0
            0x0001_00C1,
            0x3604_0281, // v_and_b32 v2, 1, v1
            0x3606_0282, // v_and_b32 v3, 2, v1
            0x7E04_0D02, // v_cvt_f32_u32 v2, v2
            0x7E06_0D03, // v_cvt_f32_u32 v3, v3
            0x1008_04F6, // v_mul_f32 v4, 4.0, v2
            0x0608_08F3, // v_add_f32 v4, -1.0, v4
            0x100A_06F4, // v_mul_f32 v5, 2.0, v3
            0x060A_0AF3, // v_add_f32 v5, -1.0, v5
            0x7E0E_02F2, // v_mov_b32 v7, 1.0
            0x7E12_02FF, // v_mov_b32 v9, triangle (0, 1, 2)
            1 << 10 | 2 << 20,
            0xF800_0941, // exp prim v9, off, off, off done
            0x0000_0009,
            0xF800_08CF, // exp pos0 v4, v5, v6, v7 done
            0x0706_0504,
            0xBF81_0000, // s_endpgm
        ];
        let native = NativeProgram {
            stage: HwStage::Ms,
            wave64: false,
            vgprs: 10,
            layout: vec![],
            used: vec![],
            io: StageIo::Ms(MeshIo {
                outputs: vec![],
                clip_cntl: 0,
                threads: LANES,
                max_vertices: LANES,
                max_primitives: LANES,
                topology: 2,
                lds_bytes: 0,
            }),
            code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        };
        // One element: SV_Position (system value 1), float, register 0, xyzw.
        let mut osg1 = Vec::new();
        for w in [1u32, 8, 0, 40, 0, 1, 3, 0, 0xF, 0] {
            osg1.extend(w.to_le_bytes());
        }
        osg1.extend(b"SV_Position\0");
        // A DXBC container of that one part (header, one offset, the part).
        let mut container = b"DXBC".to_vec();
        container.extend([0; 16]);
        for w in [1u32, (0x24 + 8 + osg1.len()) as u32, 1, 0x24] {
            container.extend(w.to_le_bytes());
        }
        container.extend(b"OSG1");
        container.extend((osg1.len() as u32).to_le_bytes());
        container.extend(&osg1);
        let c = Container::parse(&container).unwrap();
        let p = victory_shader::ir::build(&native.patched_code(), native.wave64, native.vgprs).unwrap();
        let st = victory_shader::structure::structure(&p).unwrap();
        let res = victory_shader::resource::track(&p, &native.layout, native.stage, Some(&st)).unwrap();
        let io = victory_shader::io::raise(&p, &native).unwrap();
        let options = Options {
            mesh_as_compute: true,
            ..Options::default()
        };
        let cs = victory_shader::emit::emit_with(&c, &native, &p, &st, &io, &res, options).unwrap();
        (container, cs)
    }

    fn view(kind: D3D12_ROOT_PARAMETER_TYPE, register: u32, visibility: D3D12_SHADER_VISIBILITY) -> D3D12_ROOT_PARAMETER1 {
        D3D12_ROOT_PARAMETER1 {
            ParameterType: kind,
            Anonymous: D3D12_ROOT_PARAMETER1_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                    ShaderRegister: register,
                    RegisterSpace: MESH_SPACE,
                    Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                },
            },
            ShaderVisibility: visibility,
        }
    }

    fn code(bytes: &[u8]) -> D3D12_SHADER_BYTECODE {
        D3D12_SHADER_BYTECODE {
            pShaderBytecode: bytes.as_ptr().cast(),
            BytecodeLength: bytes.len(),
        }
    }

    /// The two passes' root signatures and pipelines, and the layout.
    struct Passes {
        compute: (ID3D12RootSignature, ID3D12PipelineState),
        draw: (ID3D12RootSignature, ID3D12PipelineState),
        layout: Layout,
    }

    fn passes(host: &Host) -> Passes {
        let (container, cs) = program();
        assert_eq!(cs.user_data_dwords(), 0, "the program reads no user data");
        let vs = victory_shader::emit::mesh_vertex_shader(&container).unwrap();
        let ps = victory_shader::emit::flat_pixel_shader(&[0]);
        let all = D3D12_SHADER_VISIBILITY_ALL;
        let compute_params = [
            view(D3D12_ROOT_PARAMETER_TYPE_UAV, 0, all),
            view(D3D12_ROOT_PARAMETER_TYPE_UAV, 1, all),
            view(D3D12_ROOT_PARAMETER_TYPE_SRV, 1, all),
        ];
        let (croot, _) = crate::pipeline::host::root_signature(&host.device, &compute_params, &[], true).unwrap();
        let draw_params = [view(D3D12_ROOT_PARAMETER_TYPE_SRV, 0, D3D12_SHADER_VISIBILITY_VERTEX)];
        let (droot, _) = crate::pipeline::host::root_signature(&host.device, &draw_params, &[], false).unwrap();
        // SAFETY: descriptions pointing at live shaders and roots.
        unsafe {
            let cpso = host
                .device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&croot),
                    CS: code(&cs.dxil),
                    ..Default::default()
                })
                .unwrap();
            let mut blend = D3D12_BLEND_DESC::default();
            blend.RenderTarget[0].RenderTargetWriteMask = 0xF;
            let mut formats = [DXGI_FORMAT_UNKNOWN; 8];
            formats[0] = DXGI_FORMAT_R32G32B32A32_FLOAT;
            let dpso = host
                .device
                .CreateGraphicsPipelineState(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&droot),
                    VS: code(&vs.shader.dxil),
                    PS: code(&ps),
                    BlendState: blend,
                    SampleMask: u32::MAX,
                    RasterizerState: D3D12_RASTERIZER_DESC {
                        FillMode: D3D12_FILL_MODE_SOLID,
                        CullMode: D3D12_CULL_MODE_NONE,
                        DepthClipEnable: false.into(),
                        ..Default::default()
                    },
                    PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                    NumRenderTargets: 1,
                    RTVFormats: formats,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    ..Default::default()
                })
                .unwrap();
            Passes {
                compute: (croot, cpso),
                draw: (droot, dpso),
                layout: Layout {
                    stride: vs.stride,
                    max_vertices: LANES,
                    max_primitives: LANES,
                    compute: [0, 1, 2],
                    draw: 0,
                },
            }
        }
    }

    /// A cleared SIZE x SIZE float target, its view, and a list recording
    /// into it.
    fn target(host: &Host) -> (ID3D12Resource, ID3D12DescriptorHeap, HostList) {
        // SAFETY: plain creations; the clear names our own view.
        unsafe {
            let heap: ID3D12DescriptorHeap = host
                .device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: 1,
                    ..Default::default()
                })
                .unwrap();
            let mut made: Option<ID3D12Resource> = None;
            host.device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_DEFAULT,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &D3D12_RESOURCE_DESC {
                        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                        Width: u64::from(SIZE),
                        Height: SIZE,
                        DepthOrArraySize: 1,
                        MipLevels: 1,
                        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                        ..Default::default()
                    },
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    None,
                    &mut made,
                )
                .unwrap();
            let rt = made.unwrap();
            let rtv = heap.GetCPUDescriptorHandleForHeapStart();
            host.device.CreateRenderTargetView(&rt, None, rtv);
            let list = host.new_list().unwrap();
            list.list.ClearRenderTargetView(rtv, &[0.0; 4], None);
            (rt, heap, list)
        }
    }

    /// Binds a pass as `pipeline::host`'s would be (target, viewport).
    fn binder<'a>(list: &'a HostList, p: &'a Passes, heap: &'a ID3D12DescriptorHeap) -> impl FnMut(bool) -> bool + 'a {
        move |compute| {
            let l = &list.list;
            // SAFETY: an open list; live roots, pipelines and view.
            unsafe {
                if compute {
                    l.SetComputeRootSignature(&p.compute.0);
                    l.SetPipelineState(&p.compute.1);
                } else {
                    l.SetGraphicsRootSignature(&p.draw.0);
                    l.SetPipelineState(&p.draw.1);
                    let rtv = heap.GetCPUDescriptorHandleForHeapStart();
                    l.OMSetRenderTargets(1, Some(&rtv), false, None);
                    l.RSSetViewports(&[D3D12_VIEWPORT {
                        Width: SIZE as f32,
                        Height: SIZE as f32,
                        MaxDepth: 1.0,
                        ..Default::default()
                    }]);
                    l.RSSetScissorRects(&[windows::Win32::Foundation::RECT {
                        right: SIZE as i32,
                        bottom: SIZE as i32,
                        ..Default::default()
                    }]);
                }
            }
            true
        }
    }

    /// Copies, at the end of `list`, the target and `bytes` of `buffer`
    /// from `offset` into CPU memory; runs the list; returns the texels
    /// (flat colour or not) and the buffer's dwords.
    fn finish(host: &Host, list: HostList, rt: &ID3D12Resource, (buffer, offset, bytes): (&ID3D12Resource, u64, u64)) -> (Vec<bool>, Vec<u32>) {
        let pitch = u64::from(SIZE) * 16;
        let texels = pitch * u64::from(SIZE);
        let (out, data) = host.shared_buffer(texels + bytes).unwrap();
        // SAFETY: an open list; live resources; the memory is read after
        // the list has run.
        unsafe {
            let l = &list.list;
            l.ResourceBarrier(&[crate::host::transition(
                rt,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            l.CopyTextureRegion(
                &D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&out),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                            Offset: 0,
                            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                                Width: SIZE,
                                Height: SIZE,
                                Depth: 1,
                                RowPitch: pitch as u32,
                            },
                        },
                    },
                },
                0,
                0,
                0,
                &D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(rt),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                },
                None,
            );
            l.CopyBufferRegion(&out, texels, buffer, offset, bytes);
            list.close();
            host.execute(&[&list]);
            let f = data.cast::<f32>();
            let lit = (0..(SIZE * SIZE) as usize)
                .map(|t| (0..4).all(|c| f.add(t * 4 + c).read_volatile() == victory_shader::emit::FLAT_COLOUR[c]))
                .collect();
            let words = data.add(texels as usize).cast::<u32>();
            (lit, (0..(bytes / 4) as usize).map(|i| words.add(i).read_volatile()).collect())
        }
    }

    /// A direct dispatch of three groups in chunks of two: every texel is
    /// drawn, and the last chunk (the title's group 2, the scratch's
    /// group 0) holds group number 2 as z.
    #[test]
    fn direct_mesh_dispatch_runs_in_chunks() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let scratch = scratch().expect("mesh scratch");
        let p = passes(host);
        let (rt, heap, list) = target(host);
        assert!(run_direct(&list, scratch, &p.layout, (3, 2), binder(&list, &p, &heap)));
        let bytes = u64::from(LANES * p.layout.stride);
        let (lit, vertices) = finish(host, list, &rt, (&scratch.buffer, 0, bytes));
        assert!(lit.iter().all(|&l| l), "{} of 256 texels drawn", lit.iter().filter(|&&l| l).count());
        for v in 0..LANES as usize {
            let z = f32::from_bits(vertices[v * 4 + 2]);
            assert_eq!(z, (2 * LANES) as f32, "vertex {v}");
        }
        eprintln!("direct emulated mesh dispatch drew ({})", host.caps.adapter.name);
    }

    /// Indirect: records of 2, 0 and 1 groups and one past the count
    /// become dispatch and draw records, the groups packed in the scratch
    /// in order; every texel is drawn.
    #[test]
    fn indirect_mesh_dispatch_converts_and_draws() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let scratch = scratch().expect("mesh scratch");
        let p = passes(host);
        let device = &host.device;
        let signatures = signatures(device, (&p.compute.0, p.layout.compute), (&p.draw.0, p.layout.draw)).unwrap();
        let (args, data) = host.shared_buffer(64).unwrap();
        let records = [[2u32, 1, 1], [0, 1, 1], [1, 1, 1], [5, 1, 1]];
        // SAFETY: our mapped buffer, written before the list runs.
        unsafe {
            let words = data.cast::<u32>();
            for (i, w) in records.iter().flatten().enumerate() {
                words.add(i).write_volatile(*w);
            }
            words.add(12).write_volatile(3);
        }
        let (rt, heap, list) = target(host);
        // SAFETY: a live buffer.
        let gpu = unsafe { args.GetGPUVirtualAddress() };
        assert!(run_indirect(
            &list,
            scratch,
            &p.layout,
            (&signatures.0, &signatures.1),
            (gpu, Some(gpu + 48)),
            (12, 4),
            binder(&list, &p, &heap),
        ));
        let (lit, commands) = finish(host, list, &rt, (&scratch.commands, 0, u64::from(DRAWS_AT + 4 * DRAW_RECORD)));
        assert!(lit.iter().all(|&l| l), "{} of 256 texels drawn", lit.iter().filter(|&&l| l).count());
        // SAFETY: a live buffer.
        let base = unsafe { scratch.buffer.GetGPUVirtualAddress() };
        let group = u64::from(LANES * p.layout.stride + LANES * 12);
        let address = |at: usize| u64::from(commands[at]) | u64::from(commands[at + 1]) << 32;
        for (r, (groups, first)) in [(2, 0), (0, 2), (1, 2), (0, 3)].into_iter().enumerate() {
            let at = r * DISPATCH_RECORD as usize / 4;
            assert_eq!(address(at), base + first * group, "record {r}: vertices");
            assert_eq!(commands[at + 6], groups, "record {r}: groups");
            let at = (DRAWS_AT + r as u32 * DRAW_RECORD) as usize / 4;
            assert_eq!(address(at), base + first * group, "record {r}: drawn vertices");
            assert_eq!(commands[at + 6], groups * LANES * 3, "record {r}: indices");
        }
        eprintln!("indirect emulated mesh dispatches drew ({})", host.caps.adapter.name);
    }
}
