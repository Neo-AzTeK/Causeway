//! Host pipelines for title pipelines (decision 0007, step 3): the
//! title's graphics state with the translated shaders, under a root
//! signature of our own that the translated shaders' bindings define
//! (`victory_shader::emit`): per stage one block of root constants for the
//! user data, root views for the title's root views, static samplers for
//! the immediate samplers, and directly indexed heaps.
//!
//! Emulated mesh pipelines (added 2026-11-25): where the host has no mesh
//! shaders (`caps::mesh_shaders`), a title mesh pipeline is
//! two host pipelines ([`EmulatedMesh`]): the mesh program made as compute,
//! which writes its vertices and triangles to scratch buffers, and a
//! graphics pipeline of the vertex shader made for it with the title's
//! pixel shader and state, which draws them as an indexed triangle list
//! (`victory_shader::emit`, "Mesh shaders as compute"; the draw path is
//! `draw::State::dispatch_mesh`).

use std::ffi::{CStr, CString};
use std::sync::Arc;

use victory_shader::emit::{MEMORY_SPACE, MESH_SPACE, required_waves,ROOT_CBV_SPACE, SAMPLER_SPACE, USER_DATA_SPACE};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::PCSTR;

use super::{Shader, Translated, fallback};
use crate::caps::{
    Caps, Refusal, WaveNeed, WavePlan, aniso_point_mip, atomics64_heap, heap_indexing, mesh_shaders, wave_plan,
};
use crate::host::host;

/// A stream's graphics state, owned (the title's stream lives only for the
/// creation call). Subobjects the stream leaves out keep D3D12's defaults.
pub(crate) struct GraphicsState {
    blend: D3D12_BLEND_DESC,
    sample_mask: u32,
    rasterizer: D3D12_RASTERIZER_DESC,
    depth: D3D12_DEPTH_STENCIL_DESC,
    /// Input elements, their names pointing into `names`.
    inputs: Vec<D3D12_INPUT_ELEMENT_DESC>,
    names: Vec<CString>,
    strip_cut: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE,
    pub(crate) topology: D3D12_PRIMITIVE_TOPOLOGY_TYPE,
    pub(crate) targets: D3D12_RT_FORMAT_ARRAY,
    depth_format: DXGI_FORMAT,
    samples: DXGI_SAMPLE_DESC,
}

impl Default for GraphicsState {
    fn default() -> Self {
        let target = D3D12_RENDER_TARGET_BLEND_DESC {
            SrcBlend: D3D12_BLEND_ONE,
            DestBlend: D3D12_BLEND_ZERO,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_ZERO,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
            ..Default::default()
        };
        let keep = D3D12_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D12_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
            StencilPassOp: D3D12_STENCIL_OP_KEEP,
            StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        };
        GraphicsState {
            blend: D3D12_BLEND_DESC {
                RenderTarget: [target; 8],
                ..Default::default()
            },
            sample_mask: u32::MAX,
            rasterizer: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_BACK,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            depth: D3D12_DEPTH_STENCIL_DESC {
                DepthEnable: true.into(),
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                DepthFunc: D3D12_COMPARISON_FUNC_LESS,
                StencilReadMask: 0xff,
                StencilWriteMask: 0xff,
                FrontFace: keep,
                BackFace: keep,
                ..Default::default()
            },
            inputs: Vec::new(),
            names: Vec::new(),
            strip_cut: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
            topology: D3D12_PRIMITIVE_TOPOLOGY_TYPE_UNDEFINED,
            targets: D3D12_RT_FORMAT_ARRAY::default(),
            depth_format: DXGI_FORMAT_UNKNOWN,
            samples: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
        }
    }
}

impl GraphicsState {
    /// Takes in one subobject of a stream: its type, then its data at +4
    /// (+8 for those holding pointers).
    ///
    /// # Safety
    /// `at` points at a whole subobject of type `kind`.
    pub(crate) unsafe fn read(&mut self, kind: u32, at: *const u8) {
        // SAFETY: the caller passes a whole subobject.
        unsafe {
            let data = at.add(4);
            match kind {
                8 => self.blend = data.cast::<D3D12_BLEND_DESC>().read_unaligned(),
                9 => self.sample_mask = data.cast::<u32>().read_unaligned(),
                10 => self.rasterizer = data.cast::<D3D12_RASTERIZER_DESC>().read_unaligned(),
                11 => self.depth = data.cast::<D3D12_DEPTH_STENCIL_DESC>().read_unaligned(),
                21 => {
                    // shortcut: the depth bounds test is dropped (the host
                    // pipeline uses the plain description); bring it back
                    // with a stream if a title enables it.
                    let d = data.cast::<D3D12_DEPTH_STENCIL_DESC1>().read_unaligned();
                    self.depth = D3D12_DEPTH_STENCIL_DESC {
                        DepthEnable: d.DepthEnable,
                        DepthWriteMask: d.DepthWriteMask,
                        DepthFunc: d.DepthFunc,
                        StencilEnable: d.StencilEnable,
                        StencilReadMask: d.StencilReadMask,
                        StencilWriteMask: d.StencilWriteMask,
                        FrontFace: d.FrontFace,
                        BackFace: d.BackFace,
                    };
                }
                12 => {
                    let layout = at.add(8).cast::<D3D12_INPUT_LAYOUT_DESC>().read_unaligned();
                    if layout.pInputElementDescs.is_null() {
                        return;
                    }
                    let elements = std::slice::from_raw_parts(
                        layout.pInputElementDescs,
                        layout.NumElements as usize,
                    );
                    self.names = elements
                        .iter()
                        .map(|e| CStr::from_ptr(e.SemanticName.0.cast()).to_owned())
                        .collect();
                    self.inputs = elements
                        .iter()
                        .zip(&self.names)
                        .map(|(e, name)| D3D12_INPUT_ELEMENT_DESC {
                            SemanticName: PCSTR(name.as_ptr().cast()),
                            Format: DXGI_FORMAT(
                                crate::resource::pc_format(e.Format.0 as u32) as i32
                            ),
                            ..*e
                        })
                        .collect();
                }
                13 => {
                    self.strip_cut =
                        D3D12_INDEX_BUFFER_STRIP_CUT_VALUE(data.cast::<i32>().read_unaligned())
                }
                14 => {
                    self.topology =
                        D3D12_PRIMITIVE_TOPOLOGY_TYPE(data.cast::<i32>().read_unaligned())
                }
                15 => {
                    self.targets = data.cast::<D3D12_RT_FORMAT_ARRAY>().read_unaligned();
                    for f in &mut self.targets.RTFormats {
                        f.0 = crate::resource::pc_format(f.0 as u32) as i32;
                    }
                }
                16 => {
                    let f = data.cast::<u32>().read_unaligned();
                    self.depth_format = DXGI_FORMAT(crate::resource::pc_format(f) as i32);
                }
                17 => self.samples = data.cast::<DXGI_SAMPLE_DESC>().read_unaligned(),
                _ => {}
            }
        }
    }

    /// Everything the state holds, as text: part of a pipeline's name in
    /// the library and of the key a title pipeline is started early by.
    pub(crate) fn describe(&self) -> String {
        let inputs: Vec<String> = self
            .inputs
            .iter()
            .zip(&self.names)
            .map(|(e, name)| {
                format!(
                    "{name:?} {} {} {} {} {} {}",
                    e.SemanticIndex, e.Format.0, e.InputSlot, e.AlignedByteOffset, e.InputSlotClass.0, e.InstanceDataStepRate
                )
            })
            .collect();
        format!(
            "{:?} {} {:?} {:?} {inputs:?} {} {} {:?} {} {:?}",
            self.blend,
            self.sample_mask,
            self.rasterizer,
            self.depth,
            self.strip_cut.0,
            self.topology.0,
            self.targets,
            self.depth_format.0,
            self.samples
        )
    }

    /// The blend state the host takes: no blending on a target whose format
    /// it cannot blend (`blendable`), as integer formats everywhere and
    /// B4G4R4A4 on some GPUs. A state asking for it gets the whole pipeline
    /// refused (E_INVALIDARG) when the pixel shader writes that target.
    /// shortcut: such a target is written unblended; a host copy in a format
    /// that blends would keep the title's blending.
    fn host_blend(&self, blendable: impl Fn(DXGI_FORMAT) -> bool) -> D3D12_BLEND_DESC {
        let mut blend = self.blend;
        let count = self.targets.NumRenderTargets.min(8) as usize;
        let unblendable = |i: usize| {
            let format = self.targets.RTFormats[i];
            i < count && format != DXGI_FORMAT_UNKNOWN && !blendable(format)
        };
        if !(0..count).any(unblendable) {
            return blend;
        }
        if !blend.IndependentBlendEnable.as_bool() {
            // The first entry speaks for every target. Without blending
            // there is nothing to turn off (and it may hold a logic op,
            // which independent blending forbids).
            if !blend.RenderTarget[0].BlendEnable.as_bool() {
                return blend;
            }
            blend.RenderTarget = [blend.RenderTarget[0]; 8];
            blend.IndependentBlendEnable = true.into();
        }
        for (i, target) in blend.RenderTarget.iter_mut().enumerate() {
            if i < count && unblendable(i) {
                target.BlendEnable = false.into();
            }
        }
        blend
    }

    /// The vertex buffer slots the input layout reads.
    fn slots(&self) -> u32 {
        self.inputs
            .iter()
            .fold(0, |m, e| m | 1 << e.InputSlot.min(31))
    }

    /// The input elements with their byte offsets worked out.
    fn elements(&self) -> Vec<Input> {
        let mut next = [0u32; 32];
        self.inputs
            .iter()
            .map(|e| {
                let slot = e.InputSlot.min(31) as usize;
                let format = e.Format.0 as u32;
                let bytes = crate::resource::element(format).map_or(0, |(b, _)| b as u32);
                // An appended element starts at the next multiple of
                // min(4, its size) after the previous one, where D3D12 (and
                // vkd3d) place it; the host pipeline gets the title's
                // descriptions, so the offsets read here must match.
                let offset = if e.AlignedByteOffset == D3D12_APPEND_ALIGNED_ELEMENT {
                    next[slot]
                        .checked_next_multiple_of(bytes.clamp(1, 4))
                        .unwrap_or(u32::MAX)
                } else {
                    e.AlignedByteOffset
                };
                next[slot] = offset.saturating_add(bytes);
                Input {
                    slot: slot as u32,
                    offset,
                    format,
                    per_instance: e.InputSlotClass == D3D12_INPUT_CLASSIFICATION_PER_INSTANCE_DATA,
                }
            })
            .collect()
    }
}

/// One input element: vertex buffer slot, byte offset, format.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Input {
    pub(crate) slot: u32,
    pub(crate) offset: u32,
    pub(crate) format: u32,
    pub(crate) per_instance: bool,
}

/// The console's own primitive topology types.
const RECTANGLES: i32 = 19;
const QUADS: i32 = 32;

/// One stage of a host pipeline and where its bindings go.
pub(crate) struct Stage {
    pub(crate) shader: Arc<Shader>,
    /// The root parameter of its user data, and its size in dwords.
    pub(crate) user_data: Option<(u32, u32)>,
    /// Root parameters of its root views: (title root parameter, ours).
    pub(crate) root_cbvs: Vec<(u8, u32)>,
    pub(crate) root_srvs: Vec<(u8, u32)>,
    pub(crate) root_uavs: Vec<(u8, u32)>,
    /// Our root parameters of the title-memory page table and of the
    /// written-pages bitmap (`crate::memory`), if it reads or writes there.
    pub(crate) memory: Option<u32>,
    pub(crate) memory_dirty: Option<u32>,
    /// Our root parameter of the heap's view addresses (`crate::memory`).
    pub(crate) view_addresses: Option<u32>,
}

impl Stage {
    pub(crate) fn translated(&self) -> &Translated {
        self.shader
            .translated()
            .expect("host pipelines are made from translated shaders")
    }
}

pub(crate) struct HostPipeline {
    /// A compute pipeline (else graphics).
    pub(crate) compute: bool,
    /// A mesh pipeline (graphics without vertex input).
    pub(crate) mesh: bool,
    pub(crate) pso: ID3D12PipelineState,
    pub(crate) root: ID3D12RootSignature,
    /// Vertex, then pixel stage, or the compute stage.
    pub(crate) stages: Vec<Stage>,
    /// A graphics pipeline of the console's rectangle lists whose geometry
    /// shader completes each rectangle (`victory_shader::emit::rectangle_gs`):
    /// it draws them as triangles, one per rectangle.
    pub(crate) rectangles: bool,
    /// The vertex buffer slots it reads (bit per slot).
    pub(crate) slots: u32,
    pub(crate) inputs: Vec<Input>,
    /// Its shaders, for reports of slow submissions.
    pub(crate) name: String,
    /// Our root parameters of the view-mark table and the buffer views'
    /// written-pages bitmap, if a stage marks its writes through views
    /// (`victory_shader::emit`, "Write marks").
    pub(crate) marks: Option<(u32, u32)>,
    /// A refused pixel program replaced by the flat substitute (see
    /// [`fallback`]).
    pub(crate) fallback: bool,
    /// Our root parameters of the mesh scratch (module notes): in a mesh
    /// program made as compute the vertices' and triangles' UAVs and the
    /// first group's SRV; in the vertex shader drawing them the vertices'
    /// SRV. Empty otherwise.
    pub(crate) scratch: Vec<u32>,
    /// A mesh pipeline on a host without mesh shaders: this is its draw
    /// pass, and here is its compute pass.
    pub(crate) emulated: Option<Box<EmulatedMesh>>,
}

/// The compute pass of an emulated mesh pipeline and its scratch layout.
pub(crate) struct EmulatedMesh {
    pub(crate) compute: HostPipeline,
    /// Bytes per vertex in the scratch vertices.
    pub(crate) stride: u32,
    /// Vertex and primitive slots per group.
    pub(crate) max_vertices: u32,
    pub(crate) max_primitives: u32,
    /// The command signatures of its indirect dispatches (compute pass,
    /// draw pass; `crate::mesh::signatures`); without them those are
    /// dropped.
    pub(crate) signatures: Option<(ID3D12CommandSignature, ID3D12CommandSignature)>,
}

/// Which host pipeline [`HostPipeline::build`] makes of a title pipeline.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass<'a> {
    /// The title's stages as they are.
    Title,
    /// An emulated mesh pipeline's compute pass.
    MeshCompute,
    /// Its draw pass, with this vertex shader.
    MeshDraw(&'a [u8]),
}

/// Logs a pipeline that gets no host pipeline, once per reason.
fn refuse(reason: &'static str, shaders: &[Arc<Shader>]) -> Option<HostPipeline> {
    static SEEN: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&reason) {
        seen.push(reason);
        tracing::warn!(?shaders, "no host pipeline: {reason} (logged once)");
    }
    None
}

impl HostPipeline {
    /// A host pipeline for vertex and pixel shaders with the graphics
    /// state `g`, or for one compute shader.
    pub(crate) fn new(shaders: &[Arc<Shader>], g: Option<&GraphicsState>) -> Option<HostPipeline> {
        let made = Self::build(shaders, g, false, Pass::Title);
        if made.is_some() || !fallback::enabled() {
            return made;
        }
        let g = g?;
        let ps = shaders.iter().find(|s| s.stage == "pixel")?;
        let mut flat = Self::build(shaders, Some(g), true, Pass::Title)?;
        flat.fallback = true;
        fallback::made(ps);
        Some(flat)
    }

    /// An emulated mesh pipeline (module notes), its pixel stage the flat
    /// substitute if `flat`.
    fn emulated(shaders: &[Arc<Shader>], g: Option<&GraphicsState>, flat: bool) -> Option<HostPipeline> {
        let ms = shaders.iter().find(|s| s.stage == "mesh")?;
        let made = victory_shader::emit::mesh_vertex_shader(&ms.container)
            .and_then(|vs| Ok((vs, victory_shader::emit::mesh_limits(&ms.container)?)));
        let (vs, (max_vertices, max_primitives)) = match made {
            Ok(made) => made,
            Err(e) => {
                tracing::warn!(error = %e, ?shaders, "no vertex shader for an emulated mesh pipeline");
                return refuse("a mesh program the emulation cannot draw", shaders);
            }
        };
        let compute = Self::build(shaders, g, flat, Pass::MeshCompute)?;
        let mut draw = Self::build(shaders, g, flat, Pass::MeshDraw(&vs.shader.dxil))?;
        tracing::info!(?shaders, stride = vs.stride, max_vertices, max_primitives, "host mesh pipeline, emulated");
        let signatures = match (&compute.scratch[..], &draw.scratch[..]) {
            (&[vertices, triangles, first], &[drawn]) => crate::mesh::signatures(
                &host()?.device,
                (&compute.root, [vertices, triangles, first]),
                (&draw.root, drawn),
            )
            .inspect_err(|e| tracing::warn!(error = %e, ?shaders, "no indirect signatures for an emulated mesh pipeline"))
            .ok(),
            _ => None,
        };
        draw.emulated = Some(Box::new(EmulatedMesh {
            compute,
            stride: vs.stride,
            max_vertices,
            max_primitives,
            signatures,
        }));
        Some(draw)
    }

    /// `new`, with the pixel stage replaced by the flat substitute if
    /// `flat`; `pass` says which host pipeline of the title's.
    fn build(shaders: &[Arc<Shader>], g: Option<&GraphicsState>, flat: bool, pass: Pass) -> Option<HostPipeline> {
        let find = |stage| shaders.iter().find(|s| s.stage == stage);
        let compute = match shaders {
            [cs] if cs.stage == "compute" => Some(cs),
            _ => None,
        };
        if compute.is_none()
            && shaders
                .iter()
                .any(|s| !matches!(s.stage, "vertex" | "pixel" | "mesh"))
        {
            return refuse(
                "only vertex, mesh, pixel and compute stages are translated",
                shaders,
            );
        }
        let mesh = find("mesh");
        // Keep native mesh shaders where the caps say the host has them.
        if mesh.is_some() && pass == Pass::Title && !mesh_shaders(&host()?.caps) {
            return Self::emulated(shaders, g, flat);
        }
        let mesh_draw = matches!(pass, Pass::MeshDraw(_));
        let stages = if let Some(cs) = compute {
            vec![(cs, D3D12_SHADER_VISIBILITY_ALL)]
        } else if pass == Pass::MeshCompute {
            vec![(mesh?, D3D12_SHADER_VISIBILITY_ALL)]
        } else {
            let first = match (mesh, find("vertex")) {
                (Some(ms), _) => (ms, D3D12_SHADER_VISIBILITY_MESH),
                (None, Some(vs)) => (vs, D3D12_SHADER_VISIBILITY_VERTEX),
                (None, None) => return refuse("no vertex or mesh shader", shaders),
            };
            // The draw pass's vertex shader is ours: no title stage.
            let mut stages = if mesh_draw { Vec::new() } else { vec![first] };
            if !flat {
                stages.extend(find("pixel").map(|ps| (ps, D3D12_SHADER_VISIBILITY_PIXEL)));
            }
            stages
        };
        let as_compute = compute.is_some() || pass == Pass::MeshCompute;
        for (s, _) in &stages {
            s.translated()?;
        }
        let host = host()?;
        // Every translated stage indexes the heaps; the start log has the
        // device's figures.
        if heap_indexing(&host.caps).is_err() {
            return refuse(
                "the host GPU lacks heap-indexed descriptors (shader model 6.6 and resource binding tier 3)",
                shaders,
            );
        }
        // `SFI0`: 64-bit atomics on typed resources and on heap resources.
        let atomics64 = stages.iter().any(|(s, _)| {
            let Some(t) = s.translated() else {
                return false;
            };
            victory_shader::Container::parse(&t.host.dxil)
                .ok()
                .and_then(|c| {
                    c.part(victory_shader::FourCC::SFI0)?
                        .data
                        .get(..4)?
                        .try_into()
                        .ok()
                })
                .is_some_and(|b: [u8; 4]| u32::from_le_bytes(b) & 0x1040_0000 != 0)
        });
        if atomics64 && !atomics64_heap(&host.caps) {
            return refuse(
                "the host GPU has no 64-bit atomics on typed heap resources",
                shaders,
            );
        }

        let aniso_point_mip_supported = aniso_point_mip(&host.caps);

        // The root signature the translated shaders' bindings define.
        let mut params: Vec<D3D12_ROOT_PARAMETER1> = Vec::new();
        let mut samplers = Vec::new();
        let mut bound = Vec::new();
        for &(shader, visibility) in &stages {
            let t = &shader.translated()?.host;
            let mut stage = Stage {
                shader: shader.clone(),
                user_data: None,
                root_cbvs: Vec::new(),
                root_srvs: Vec::new(),
                root_uavs: Vec::new(),
                memory: None,
                memory_dirty: None,
                view_addresses: None,
            };
            let dwords = t.user_data_dwords();
            if dwords > 0 {
                stage.user_data = Some((params.len() as u32, dwords));
                params.push(D3D12_ROOT_PARAMETER1 {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                    Anonymous: D3D12_ROOT_PARAMETER1_0 {
                        Constants: D3D12_ROOT_CONSTANTS {
                            ShaderRegister: 0,
                            RegisterSpace: USER_DATA_SPACE,
                            Num32BitValues: dwords,
                        },
                    },
                    ShaderVisibility: visibility,
                });
            }
            for (slots, kind, out) in [
                (
                    &t.root_cbvs,
                    D3D12_ROOT_PARAMETER_TYPE_CBV,
                    &mut stage.root_cbvs,
                ),
                (
                    &t.root_srvs,
                    D3D12_ROOT_PARAMETER_TYPE_SRV,
                    &mut stage.root_srvs,
                ),
                (
                    &t.root_uavs,
                    D3D12_ROOT_PARAMETER_TYPE_UAV,
                    &mut stage.root_uavs,
                ),
            ] {
                for &slot in slots {
                    out.push((slot, params.len() as u32));
                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: kind,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                                ShaderRegister: u32::from(slot),
                                RegisterSpace: ROOT_CBV_SPACE,
                                Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                            },
                        },
                        ShaderVisibility: visibility,
                    });
                }
            }
            for (used, kind, register, out) in [
                (t.memory, D3D12_ROOT_PARAMETER_TYPE_SRV, 0, &mut stage.memory),
                (t.memory_writes, D3D12_ROOT_PARAMETER_TYPE_UAV, 0, &mut stage.memory_dirty),
                (t.view_addresses, D3D12_ROOT_PARAMETER_TYPE_SRV, 1, &mut stage.view_addresses),
            ] {
                if used {
                    *out = Some(params.len() as u32);
                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: kind,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                                ShaderRegister: register,
                                RegisterSpace: MEMORY_SPACE,
                                Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                            },
                        },
                        ShaderVisibility: visibility,
                    });
                }
            }
            for (n, s) in t.static_samplers.iter().enumerate() {
                samplers.push(D3D12_STATIC_SAMPLER_DESC {
                    Filter: D3D12_FILTER(s.filter_for_host(aniso_point_mip_supported) as i32),
                    AddressU: D3D12_TEXTURE_ADDRESS_MODE(s.address[0] as i32),
                    AddressV: D3D12_TEXTURE_ADDRESS_MODE(s.address[1] as i32),
                    AddressW: D3D12_TEXTURE_ADDRESS_MODE(s.address[2] as i32),
                    MipLODBias: s.mip_lod_bias,
                    MaxAnisotropy: s.max_anisotropy,
                    ComparisonFunc: D3D12_COMPARISON_FUNC(s.comparison as i32),
                    BorderColor: D3D12_STATIC_BORDER_COLOR(s.border as i32),
                    MinLOD: s.min_lod,
                    MaxLOD: s.max_lod,
                    ShaderRegister: n as u32,
                    RegisterSpace: SAMPLER_SPACE,
                    ShaderVisibility: visibility,
                });
            }
            bound.push(stage);
        }
        // One pair for all stages: they share the heap and the bitmap.
        let marks = stages
            .iter()
            .any(|(s, _)| s.translated().is_some_and(|t| t.host.view_marks))
            .then(|| {
                let first = params.len() as u32;
                for (kind, register) in [
                    (D3D12_ROOT_PARAMETER_TYPE_SRV, victory_shader::emit::VIEW_MARKS),
                    (D3D12_ROOT_PARAMETER_TYPE_UAV, victory_shader::emit::VIEW_WRITES),
                ] {
                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: kind,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                                ShaderRegister: register,
                                RegisterSpace: MEMORY_SPACE,
                                Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                            },
                        },
                        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    });
                }
                (first, first + 1)
            });
        // The mesh scratch (module notes; `victory_shader::emit::MESH_SPACE`).
        let views: &[(D3D12_ROOT_PARAMETER_TYPE, u32, D3D12_SHADER_VISIBILITY)] = match pass {
            Pass::Title => &[],
            Pass::MeshCompute => &[
                (D3D12_ROOT_PARAMETER_TYPE_UAV, 0, D3D12_SHADER_VISIBILITY_ALL),
                (D3D12_ROOT_PARAMETER_TYPE_UAV, 1, D3D12_SHADER_VISIBILITY_ALL),
                (D3D12_ROOT_PARAMETER_TYPE_SRV, 1, D3D12_SHADER_VISIBILITY_ALL),
            ],
            Pass::MeshDraw(_) => &[(D3D12_ROOT_PARAMETER_TYPE_SRV, 0, D3D12_SHADER_VISIBILITY_VERTEX)],
        };
        let mut scratch = Vec::new();
        for &(kind, register, visibility) in views {
            scratch.push(params.len() as u32);
            params.push(D3D12_ROOT_PARAMETER1 {
                ParameterType: kind,
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                        ShaderRegister: register,
                        RegisterSpace: MESH_SPACE,
                        Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                    },
                },
                ShaderVisibility: visibility,
            });
        }
        let (root, root_blob) = match root_signature(&host.device, &params, &samplers, as_compute) {
            Ok(root) => root,
            Err(e) => {
                tracing::warn!(error = %e, ?shaders, "host root signature refused");
                return None;
            }
        };
        // The substitute pixel shader, when the title's is left out.
        let flat_ps = if flat { g.map(fallback::pixel_shader).unwrap_or_default() } else { Vec::new() };
        let code = |i: usize| {
            if i == 1 && !flat_ps.is_empty() {
                return D3D12_SHADER_BYTECODE {
                    pShaderBytecode: flat_ps.as_ptr().cast(),
                    BytecodeLength: flat_ps.len(),
                };
            }
            // The draw pass: our vertex shader, then the title's stages.
            let i = match pass {
                Pass::MeshDraw(vs) if i == 0 => {
                    return D3D12_SHADER_BYTECODE {
                        pShaderBytecode: vs.as_ptr().cast(),
                        BytecodeLength: vs.len(),
                    };
                }
                Pass::MeshDraw(_) => i - 1,
                _ => i,
            };
            stages.get(i).and_then(|(s, _)| s.translated()).map_or(
                D3D12_SHADER_BYTECODE::default(),
                |t| D3D12_SHADER_BYTECODE {
                    pShaderBytecode: t.host.dxil.as_ptr().cast(),
                    BytecodeLength: t.host.dxil.len(),
                },
            )
        };
        if as_compute {
            // Refuse before creating: the host answers a wave size it lacks
            // with E_INVALIDARG, which says nothing.
            if let Some(wave) = wave_refusal(&host.caps, &code_bytes(&code(0))) {
                // What stops it running as two waves of 32.
                let why = stages
                    .first()
                    .and_then(|(s, _)| s.translated())
                    .and_then(|t| t.host.wave64_why.as_deref());
                // `wave` stays last: `xtask` reads it to the end of the line.
                tracing::warn!(?shaders, why, %wave, "host pipeline refused: wave size");
                return None;
            }
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                // SAFETY: a borrowed pointer the description never releases.
                pRootSignature: unsafe { std::mem::transmute_copy(&root) },
                CS: code(0),
                ..Default::default()
            };
            let name = super::library::name("compute", &root_blob, &[&code_bytes(&code(0))], "");
            let made = super::library::pipeline(
                &name,
                // SAFETY: the description points at live data.
                |library, name| unsafe { library.LoadComputePipeline(name, &desc) },
                // SAFETY: as above.
                || unsafe { host.device.CreateComputePipelineState(&desc) },
            );
            let pso = match made {
                Ok(pso) => pso,
                Err(e) => {
                    let wave = wave_refusal(&host.caps, &code_bytes(&code(0))).map(|r| r.to_string());
                    tracing::warn!(error = %e, ?shaders, wave, "host pipeline refused");
                    host.log_messages();
                    return None;
                }
            };
            tracing::info!(?shaders, "host pipeline");
            crate::host::name(&pso, || format!("{shaders:?}"));
            return Some(HostPipeline {
                compute: true,
                mesh: false,
                pso,
                root,
                stages: bound,
                name: format!("{shaders:?}"),
                rectangles: false,
                slots: 0,
                inputs: Vec::new(),
                marks,
                fallback: false,
                scratch,
                emulated: None,
            });
        }
        let Some(g) = g else {
            return refuse("graphics state not kept (plain description)", shaders);
        };
        let state = g.describe();
        if mesh.is_some() && !mesh_draw {
            let name = super::library::name(
                "mesh",
                &root_blob,
                &[&code_bytes(&code(0)), &code_bytes(&code(1))],
                &state,
            );
            let pso = match mesh_pipeline(&host.device, &root, code(0), code(1), g, &name) {
                Ok(pso) => pso,
                Err(e) => {
                    tracing::warn!(error = %e, ?shaders, "host mesh pipeline refused");
                    host.log_messages();
                    return None;
                }
            };
            tracing::info!(?shaders, "host mesh pipeline");
            crate::host::name(&pso, || format!("{shaders:?}"));
            return Some(HostPipeline {
                compute: false,
                mesh: true,
                pso,
                root,
                stages: bound,
                name: format!("{shaders:?}"),
                rectangles: false,
                slots: 0,
                inputs: Vec::new(),
                marks,
                fallback: false,
                scratch,
                emulated: None,
            });
        }

        // Rectangle lists: a geometry shader made for the vertex shader adds
        // each rectangle's fourth corner (else rectangles are expanded on
        // the CPU from vertex inputs, see `draw`).
        let gs = match (g.topology.0, stages.first().and_then(|(s, _)| s.translated())) {
            _ if mesh_draw => Vec::new(),
            (RECTANGLES, Some(vs)) => match victory_shader::emit::rectangle_gs(&vs.host.dxil) {
                Ok(gs) => gs,
                Err(e) => {
                    tracing::warn!(error = %e, ?shaders, "no rectangle geometry shader");
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            // SAFETY: a borrowed pointer the description never releases.
            pRootSignature: unsafe { std::mem::transmute_copy(&root) },
            VS: code(0),
            PS: code(1),
            GS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: if gs.is_empty() { std::ptr::null() } else { gs.as_ptr().cast() },
                BytecodeLength: gs.len(),
            },
            BlendState: g.host_blend(blendable(&host.device)),
            SampleMask: g.sample_mask,
            RasterizerState: g.rasterizer,
            DepthStencilState: g.depth,
            // The draw pass reads no vertex input; it draws a triangle list.
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: g.inputs.as_ptr(),
                NumElements: if mesh_draw { 0 } else { g.inputs.len() as u32 },
            },
            IBStripCutValue: if mesh_draw { D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED } else { g.strip_cut },
            PrimitiveTopologyType: if mesh_draw { D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE } else { host_topology(g) },
            NumRenderTargets: g.targets.NumRenderTargets,
            RTVFormats: g.targets.RTFormats,
            DSVFormat: g.depth_format,
            SampleDesc: g.samples,
            ..Default::default()
        };
        let name = super::library::name(
            "graphics",
            &root_blob,
            &[&code_bytes(&code(0)), &code_bytes(&code(1)), &gs],
            &state,
        );
        let started = std::time::Instant::now();
        let made = super::library::pipeline(
            &name,
            // SAFETY: the description points at live data.
            |library, name| unsafe { library.LoadGraphicsPipeline(name, &desc) },
            // SAFETY: as above.
            || unsafe { host.device.CreateGraphicsPipelineState(&desc) },
        );
        let pso = match made {
            Ok(pso) => pso,
            Err(e) => {
                // The emulated draw pass: what of the pixel shader's inputs
                // our vertex shader does not output as it reads them.
                let unlinked = if mesh_draw {
                    unlinked(&code_bytes(&code(0)), &code_bytes(&code(1)))
                } else {
                    Vec::new()
                };
                tracing::warn!(error = %e, ?shaders, ?unlinked, "host pipeline refused");
                host.log_messages();
                return None;
            }
        };
        tracing::info!(?shaders, ms = started.elapsed().as_millis(), rectangles = !gs.is_empty(), "host pipeline");
        crate::host::name(&pso, || format!("{shaders:?}"));
        tracing::debug!(
            ?shaders,
            depth = ?g.depth,
            dsv = g.depth_format.0,
            targets = ?&g.targets.RTFormats[..g.targets.NumRenderTargets.min(8) as usize],
            blend = ?g.blend.RenderTarget[0],
            cull = g.rasterizer.CullMode.0,
            "host graphics state"
        );
        Some(HostPipeline {
            compute: false,
            mesh: mesh_draw,
            pso,
            root,
            stages: bound,
            name: format!("{shaders:?}"),
            rectangles: !gs.is_empty(),
            slots: if mesh_draw { 0 } else { g.slots() },
            fallback: false,
            inputs: if mesh_draw { Vec::new() } else { g.elements() },
            marks,
            scratch,
            emulated: None,
        })
    }
}

/// The pixel inputs of `ps` that the outputs of `vs` do not cover (same
/// semantic, component type, register and components), as `name index
/// type T rRow mMask`; system
/// values the rasteriser makes are left out. Empty if a signature does not
/// parse.
fn unlinked(vs: &[u8], ps: &[u8]) -> Vec<String> {
    use victory_shader::{Container, FourCC};
    /// Name, index, system value, component type, register, mask.
    type Element = (String, u32, u32, u32, u32, u8);
    let elements = |c: &[u8], part: FourCC| -> Option<Vec<Element>> {
        let c = Container::parse(c).ok()?;
        let d = c.part(part)?.data;
        let word = |at: usize| Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?));
        (0..word(0)? as usize)
            .map(|i| {
                let at = word(4)? as usize + 32 * i;
                let name = d.get(word(at + 4)? as usize..)?.split(|&b| b == 0).next()?;
                let name = String::from_utf8_lossy(name).into_owned();
                Some((name, word(at + 8)?, word(at + 12)?, word(at + 16)?, word(at + 20)?, *d.get(at + 24)?))
            })
            .collect()
    };
    let (Some(outs), Some(ins)) = (elements(vs, FourCC::OSG1), elements(ps, FourCC::ISG1)) else {
        return Vec::new();
    };
    ins.iter()
        // Arbitrary semantics, position, clip and cull distances come from
        // the vertex shader; the rest (primitive id, front face...) not.
        .filter(|i| matches!(i.2, 0..=3))
        .filter(|i| {
            !outs.iter().any(|o| {
                o.0.eq_ignore_ascii_case(&i.0) && o.1 == i.1 && o.3 == i.3 && o.4 == i.4 && o.5 & i.5 == i.5
            })
        })
        .map(|i| format!("{} {} type {} r{} m{:#x}", i.0, i.1, i.3, i.4, i.5))
        .collect()
}

/// The host's topology type: the console's rectangle and quad types draw
/// as triangles on the host (see `draw`).
fn host_topology(g: &GraphicsState) -> D3D12_PRIMITIVE_TOPOLOGY_TYPE {
    match g.topology.0 {
        RECTANGLES | QUADS => D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
        _ => g.topology,
    }
}

/// Whether `device` blends a render target of a format. With no answer the
/// title's state is kept: creating the pipeline is then the test.
fn blendable(device: &ID3D12Device) -> impl Fn(DXGI_FORMAT) -> bool {
    |format| {
        crate::caps::per_resource::format_support(device, format)
            .is_none_or(|s| s.0 & D3D12_FORMAT_SUPPORT1_BLENDABLE.0 != 0)
    }
}

/// A mesh pipeline from the title's graphics state: the pipeline-state
/// stream form (no input layout).
fn mesh_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    ms: D3D12_SHADER_BYTECODE,
    ps: D3D12_SHADER_BYTECODE,
    g: &GraphicsState,
    name: &str,
) -> windows::core::Result<ID3D12PipelineState> {
    use windows::core::Interface;
    let d3d = device;
    /// A stream subobject: its type, then its description, pointer-aligned.
    #[repr(C, align(8))]
    struct Sub<T>(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE, T);
    #[repr(C)]
    struct Stream {
        root: Sub<*mut std::ffi::c_void>,
        ms: Sub<D3D12_SHADER_BYTECODE>,
        ps: Sub<D3D12_SHADER_BYTECODE>,
        blend: Sub<D3D12_BLEND_DESC>,
        mask: Sub<u32>,
        raster: Sub<D3D12_RASTERIZER_DESC>,
        depth: Sub<D3D12_DEPTH_STENCIL_DESC>,
        targets: Sub<D3D12_RT_FORMAT_ARRAY>,
        dsv: Sub<DXGI_FORMAT>,
        samples: Sub<DXGI_SAMPLE_DESC>,
        topology: Sub<D3D12_PRIMITIVE_TOPOLOGY_TYPE>,
    }
    let mut stream = Stream {
        root: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE, root.as_raw()),
        ms: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS, ms),
        ps: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS, ps),
        blend: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, g.host_blend(blendable(d3d))),
        mask: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, g.sample_mask),
        raster: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER, g.rasterizer),
        depth: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL, g.depth),
        targets: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS, g.targets),
        dsv: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT, g.depth_format),
        samples: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC, g.samples),
        // The type the host derives from the mesh shader anyway, given: a
        // pipeline library compares descriptions with it, and loads none
        // made from a stream without it.
        topology: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY, host_topology(g)),
    };
    let desc = D3D12_PIPELINE_STATE_STREAM_DESC {
        SizeInBytes: size_of::<Stream>(),
        pPipelineStateSubobjectStream: (&raw mut stream).cast(),
    };
    let device = device.cast::<ID3D12Device2>()?;
    super::library::pipeline(
        name,
        // SAFETY: the stream holds whole subobjects pointing at live data.
        |library, name| unsafe { library.LoadPipeline(name, &desc) },
        // SAFETY: as above.
        || unsafe { device.CreatePipelineState(&desc) },
    )
}

/// Why this host cannot run a compute shader whose container requires a wave
/// size (a console wave64 program whose result depends on its lanes), by the
/// caps' wave plan. Assumes the probed wave range and, for a host that picks
/// its own size within a range, shader model 6.6 pinning (the shader carries
/// its own `[WaveSize]`). An unknown range refuses (planner rule 1).
fn wave_refusal(caps: &Caps, container: &[u8]) -> Option<Refusal> {
    let (min, max) = required_waves(container)?;
    match wave_plan(caps, WaveNeed::Lanes { min, max, pinnable: true }) {
        WavePlan::Refuse(r) => Some(r),
        WavePlan::Native | WavePlan::Pinned(_) => None,
    }
}

/// The bytes of shader code (none for an absent stage).
fn code_bytes(code: &D3D12_SHADER_BYTECODE) -> Vec<u8> {
    if code.pShaderBytecode.is_null() {
        return Vec::new();
    }
    // SAFETY: a translation's own bytes, alive while its shader is.
    unsafe { std::slice::from_raw_parts(code.pShaderBytecode.cast::<u8>(), code.BytecodeLength) }.to_vec()
}

pub(crate) fn root_signature(
    device: &ID3D12Device,
    params: &[D3D12_ROOT_PARAMETER1],
    samplers: &[D3D12_STATIC_SAMPLER_DESC],
    compute: bool,
) -> windows::core::Result<(ID3D12RootSignature, Vec<u8>)> {
    let input = if compute {
        D3D12_ROOT_SIGNATURE_FLAG_NONE
    } else {
        D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
    };
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: windows::Win32::Graphics::Direct3D12::D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
                NumStaticSamplers: samplers.len() as u32,
                pStaticSamplers: samplers.as_ptr(),
                Flags: input
                    | D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED
                    | D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED,
            },
        },
    };
    // SAFETY: the description points at live arrays; the blob is read in
    // full before it is released.
    unsafe {
        let mut blob: Option<ID3DBlob> = None;
        let mut error: Option<ID3DBlob> = None;
        if let Err(e) = D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) {
            if let Some(error) = error {
                let text = std::slice::from_raw_parts(
                    error.GetBufferPointer().cast::<u8>(),
                    error.GetBufferSize(),
                );
                tracing::warn!("root signature: {}", String::from_utf8_lossy(text));
            }
            return Err(e);
        }
        let blob = blob.ok_or_else(windows::core::Error::empty)?;
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        Ok((device.CreateRootSignature(0, bytes)?, bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container whose runtime information requires `min` to `max` lanes.
    fn container(min: u32, max: u32) -> Vec<u8> {
        let mut c = b"DXBC".to_vec();
        c.resize(28, 0);
        // Two parts: an empty one, then the runtime information.
        for n in [2u32, 40, 48] {
            c.extend(n.to_le_bytes());
        }
        c.extend(b"SFI0\0\0\0\0PSV0");
        c.extend(52u32.to_le_bytes());
        c.extend(48u32.to_le_bytes());
        let info = c.len();
        c.resize(info + 48, 0);
        c[info + 16..info + 20].copy_from_slice(&min.to_le_bytes());
        c[info + 20..info + 24].copy_from_slice(&max.to_le_bytes());
        c
    }

    /// A state as the title's streams can hold it: blending on (`ONE`, `ONE`,
    /// `MAX`) for a float, a 4-bit and an integer target.
    #[allow(clippy::field_reassign_with_default)]
    fn blending_with_an_integer_target() -> GraphicsState {
        let mut g = GraphicsState::default();
        g.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        g.depth.DepthEnable = false.into();
        g.targets.NumRenderTargets = 3;
        g.targets.RTFormats[..3].copy_from_slice(&[
            DXGI_FORMAT_R32_FLOAT,
            DXGI_FORMAT_B4G4R4A4_UNORM,
            DXGI_FORMAT_R8_UINT,
        ]);
        g.blend.IndependentBlendEnable = true.into();
        for target in &mut g.blend.RenderTarget[..3] {
            target.BlendEnable = true.into();
            target.DestBlend = D3D12_BLEND_ONE;
            target.BlendOp = D3D12_BLEND_OP_MAX;
        }
        g
    }

    fn blending(blend: &D3D12_BLEND_DESC) -> [bool; 4] {
        std::array::from_fn(|i| blend.RenderTarget[i].BlendEnable.as_bool())
    }

    /// With fake format support: what blends, what does not, and a host
    /// with every format blendable (the title's state is kept).
    #[test]
    fn only_unblendable_targets_lose_blending() {
        let mut g = blending_with_an_integer_target();
        let integer_only = |f| f != DXGI_FORMAT_R8_UINT;
        let nor_4444 = |f| f != DXGI_FORMAT_R8_UINT && f != DXGI_FORMAT_B4G4R4A4_UNORM;
        assert_eq!(blending(&g.host_blend(integer_only)), [true, true, false, false]);
        assert_eq!(blending(&g.host_blend(nor_4444)), [true, false, false, false]);
        assert_eq!(blending(&g.host_blend(|_| true)), [true, true, true, false]);

        // One entry for every target: written out so one can differ.
        g.blend.IndependentBlendEnable = false.into();
        let host = g.host_blend(integer_only);
        assert!(host.IndependentBlendEnable.as_bool());
        assert_eq!(blending(&host), [true, true, false, true]);
        assert_eq!(host.RenderTarget[1].BlendOp, D3D12_BLEND_OP_MAX);

        // Without blending that entry is left as it is (a logic op needs it).
        g.blend.RenderTarget[0].BlendEnable = false.into();
        assert!(!g.host_blend(integer_only).IndependentBlendEnable.as_bool());

        // No integer target in use: the title's state, unchanged.
        g.blend.RenderTarget[0].BlendEnable = true.into();
        g.targets.NumRenderTargets = 2;
        assert!(!g.host_blend(integer_only).IndependentBlendEnable.as_bool());
    }

    /// The host refuses (E_INVALIDARG) blending on a target it cannot blend
    /// that the pixel shader writes: a title state that asks for it still
    /// makes its mesh and vertex pipelines. On the adapter `host()` takes; the mesh half needs a mesh
    /// tier.
    #[cfg(windows)]
    #[test]
    fn an_integer_target_with_blending_makes_its_pipelines() {
        let Some(host) = host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let made = |stage, shader: victory_shader::emit::HostShader| {
            let s = Arc::new(Shader::new(stage, stage.as_bytes()));
            s.translated
                .set(Some(Translated { host: shader, used: Vec::new() }))
                .ok()
                .unwrap();
            s
        };
        // It writes every target: one not written is accepted either way.
        let ps = victory_shader::emit::HostShader {
            dxil: victory_shader::emit::flat_pixel_shader(&[0, 1, 2]),
            ..victory_shader::emit::host::stub_ps()
        };
        let g = blending_with_an_integer_target();

        let vs = victory_shader::emit::host::stub_vs(&ps).unwrap();
        let shaders = [made("vertex", vs), made("pixel", ps.clone())];
        let vertex = HostPipeline::new(&shaders, Some(&g)).is_some();

        let mesh = if crate::caps::mesh_shaders(&host.caps) {
            let (_, ms) = crate::mesh::gpu_tests::program(false);
            let shaders = [made("mesh", ms), made("pixel", ps)];
            HostPipeline::new(&shaders, Some(&g)).is_some()
        } else {
            eprintln!("mesh half skipped: no mesh tier");
            true
        };
        assert_eq!((vertex, mesh), (true, true), "(vertex, mesh) pipelines made");
    }

    /// A pipeline is refused when its shader requires a wave size the host
    /// lacks, and only then: fake caps for wave32, wave64, 32..64 with and
    /// without pinning, and an unknown range.
    #[test]
    fn wave_sizes_the_host_lacks_are_refused() {
        use crate::caps::{ShaderModel, reference};
        let (wave64, wave32, any) = (container(64, 64), container(32, 32), container(0, u32::MAX));
        let turing = reference::nvidia_modern();
        let gcn = reference::amd_gcn();
        let mut rdna = reference::amd_rdna();
        let mut unknown = reference::amd_rdna();
        unknown.wave = None;
        // The owner's wave32-only card: as before, wave64 is refused.
        let r = wave_refusal(&turing, &wave64).expect("refused").to_string();
        assert!(r.contains("64 to 64") && r.contains("32 to 32"), "{r}");
        assert!(wave_refusal(&turing, &wave32).is_none());
        assert!(wave_refusal(&turing, &any).is_none());
        // Every NVIDIA class runs waves of 32 only.
        for caps in reference::nvidia_generations().into_iter().chain([reference::nvidia_maxwell1()]) {
            assert!(wave_refusal(&caps, &wave64).is_some(), "{}", caps.adapter.name);
            assert!(wave_refusal(&caps, &wave32).is_none() && wave_refusal(&caps, &any).is_none());
        }
        // Wave64 only: the mirror image.
        assert!(wave_refusal(&gcn, &wave64).is_none());
        assert!(wave_refusal(&gcn, &wave32).is_some());
        // 32..64 with pinning (SM 6.6): both served.
        rdna.shader_model = Some(ShaderModel::SM6_6);
        assert!(wave_refusal(&rdna, &wave64).is_none());
        assert!(wave_refusal(&rdna, &wave32).is_none());
        // 32..64 without pinning: a clean refusal, not E_INVALIDARG.
        rdna.shader_model = Some(ShaderModel(0x65));
        assert!(wave_refusal(&rdna, &wave64).is_some());
        // Unknown range: refuse what needs a size, run what does not.
        assert!(wave_refusal(&unknown, &wave64).is_some());
        assert!(wave_refusal(&unknown, &any).is_none());
        for n in 0..wave64.len() {
            // Cut short: no panic.
            let _ = required_waves(&wave64[..n]);
        }
    }

    fn element(format: DXGI_FORMAT, offset: u32) -> D3D12_INPUT_ELEMENT_DESC {
        D3D12_INPUT_ELEMENT_DESC {
            SemanticName: PCSTR(c"TEXCOORD".as_ptr().cast()),
            SemanticIndex: 0,
            Format: format,
            InputSlot: 0,
            AlignedByteOffset: offset,
            InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        }
    }

    fn offsets(inputs: &[D3D12_INPUT_ELEMENT_DESC]) -> Vec<u32> {
        let state = GraphicsState {
            inputs: inputs.to_vec(),
            ..GraphicsState::default()
        };
        state.elements().iter().map(|e| e.offset).collect()
    }

    /// An appended element after one of 1 or 2 bytes started right after
    /// it; D3D12 aligns it to min(4, its size), so the rectangle expansion
    /// read its vertices from the wrong bytes.
    #[test]
    fn appended_elements_are_aligned() {
        const APPEND: u32 = D3D12_APPEND_ALIGNED_ELEMENT;
        // RG8 (2 bytes) then RG32 float: 4, not 2.
        let a = [
            element(DXGI_FORMAT_R8G8_UNORM, 0),
            element(DXGI_FORMAT_R32G32_FLOAT, APPEND),
        ];
        assert_eq!(offsets(&a), [0, 4]);
        // R8 then R16 float: 2, not 1; then R8 packs right after it.
        let b = [
            element(DXGI_FORMAT_R8_UNORM, 0),
            element(DXGI_FORMAT_R16_FLOAT, APPEND),
            element(DXGI_FORMAT_R8_UNORM, APPEND),
        ];
        assert_eq!(offsets(&b), [0, 2, 4]);
        // Already aligned layouts are unchanged.
        let c = [
            element(DXGI_FORMAT_R8G8B8A8_UNORM, 0),
            element(DXGI_FORMAT_R32G32_FLOAT, APPEND),
            element(DXGI_FORMAT_R32G32B32_FLOAT, APPEND),
        ];
        assert_eq!(offsets(&c), [0, 4, 12]);
    }

    /// An explicit offset near 4 GiB overflowed the next appended offset
    /// (a panic in debug builds).
    #[test]
    fn a_huge_explicit_offset_does_not_overflow() {
        let inputs = [
            element(DXGI_FORMAT_R32G32B32A32_FLOAT, u32::MAX - 8),
            element(DXGI_FORMAT_R32_FLOAT, D3D12_APPEND_ALIGNED_ELEMENT),
        ];
        let _ = offsets(&inputs);
    }

    /// A pixel program the host cannot run (it does not translate): without
    /// the switch the pipeline is refused; with it, the vertex stage keeps
    /// its pipeline, a flat substitute takes the pixel stage, and the
    /// refusal is still counted. Software adapter, so it needs the
    /// `diagnostics` feature: `cargo test -p victory-gpu --features
    /// diagnostics refused_pixel`.
    #[cfg(all(windows, feature = "diagnostics"))]
    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn a_refused_pixel_program_is_drawn_with_a_substitute() {
        // SAFETY: set before any thread of this test binary reads it; the
        // host device is made on first use below.
        unsafe { std::env::set_var("VICTORY_D3D12_WARP", "1") };
        if host().is_none() {
            return;
        }
        let vs = Arc::new(Shader::new("vertex", b"a vertex shader"));
        let host_vs = victory_shader::emit::host::stub_vs(&victory_shader::emit::host::stub_ps()).unwrap();
        vs.translated
            .set(Some(Translated {
                host: host_vs,
                used: Vec::new(),
            }))
            .ok()
            .unwrap();
        let ps = Arc::new(Shader::new("pixel", b"not a console shader"));
        assert!(ps.translated().is_none());
        let mut g = GraphicsState::default();
        g.topology = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        g.targets.NumRenderTargets = 1;
        g.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        g.depth_format = DXGI_FORMAT_D32_FLOAT;
        let shaders = [vs, ps];

        fallback::force(false);
        assert!(HostPipeline::new(&shaders, Some(&g)).is_none(), "refused without the switch");
        assert_eq!(fallback::counts(), (0, 0, 0));

        fallback::force(true);
        let p = HostPipeline::new(&shaders, Some(&g)).expect("a substitute pipeline");
        fallback::force(false);
        assert!(p.fallback && !p.compute && p.stages.len() == 1);
        assert_eq!(fallback::counts(), (1, 1, 0), "still counted as a refusal");
        fallback::drew();
        assert_eq!(fallback::counts().2, 1);
        // The title's own state is kept.
        assert_eq!(p.stages[0].shader.stage, "vertex");
    }
}
