//! The vertex shader that draws what a mesh program made as compute wrote
//! (module notes, "Mesh shaders as compute"): vertex `SV_VertexID` read
//! from the scratch vertices, every element of the mesh program's output
//! signature output as the mesh shader would, so the title's pixel shader
//! links to it unchanged.

use super::HostShader;
use super::bitcode::{Attr, Bin, Cast, Term};
use super::container::SigElement;
use super::shader::{MESH_ELEMENT_BYTES, Shader};
use crate::ShaderError;
use crate::dxbc::{Container, FourCC};
use crate::native::{HwStage, StageIo};

/// The vertex shader for a mesh program made as compute, and the bytes
/// per vertex in the scratch vertices it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshVertexShader {
    /// Reads `t0` in [`super::MESH_SPACE`] (a raw root SRV), nothing else.
    pub shader: HostShader,
    pub stride: u32,
}

/// The vertex and primitive slots per group of the mesh program of the
/// console container `container`: its declared maxima.
pub fn mesh_limits(container: &[u8]) -> Result<(u32, u32), ShaderError> {
    let c = Container::parse(container)?;
    let dxil = c
        .part(FourCC::DXIL)
        .or(c.part(FourCC::ACCC))
        .ok_or(ShaderError::Malformed("no DXIL part"))?;
    let Some(StageIo::Ms(ms)) = crate::native_programs(dxil.data)?
        .into_iter()
        .find(|p| p.stage == HwStage::Ms)
        .map(|p| p.io)
    else {
        return Err(ShaderError::Unsupported {
            offset: 0,
            what: "a container without a mesh program",
        });
    };
    Ok((ms.max_vertices, ms.max_primitives))
}

/// The vertex shader for the mesh program of the console container
/// `container` (its output signature, as the translation declares it).
pub fn mesh_vertex_shader(container: &[u8]) -> Result<MeshVertexShader, ShaderError> {
    let c = Container::parse(container)?;
    let mut outputs = match c.part(FourCC::OSG1) {
        Some(part) => super::signature(part.data)?,
        None => Vec::new(),
    };
    // As the compute translation lays them out: one 16-byte slot per
    // element, whether or not it has a register.
    let stride = outputs.len() as u32 * MESH_ELEMENT_BYTES;
    for e in &mut outputs {
        e.interp = super::default_interp(e.kind, e.comp);
        e.usage = (1 << e.cols) - 1;
    }
    let mut s = Shader::new(false);
    let b = s.entry;
    let vertex_id = SigElement {
        name: "SV_VertexID".into(),
        index: 0,
        kind: 1,
        comp: 5,
        interp: 0,
        row: Some(0),
        col: 0,
        cols: 1,
        usage: 1,
        half: false,
    };
    let iu = s.m.undef(s.t.i32);
    let args = [s.m.i32(0), s.m.i32(0), s.m.i8(0), iu];
    let id = s.op(b, "loadInput.i32", Attr::ReadNone, s.t.i32, 4, &args);
    let k = s.m.i32(stride);
    let base = s.m.bin(b, Bin::Mul, id, k);
    let h = s.mesh_vertices_handle();
    for (n, e) in outputs.iter().enumerate() {
        if e.row.is_none() {
            continue;
        }
        for col in 0..e.cols {
            let off = s.m.i32(n as u32 * MESH_ELEMENT_BYTES + u32::from(col) * 4);
            let at = s.m.bin(b, Bin::Add, base, off);
            let (mask, align) = (s.m.i8(1), s.m.i32(4));
            let r = s.op(
                b,
                "rawBufferLoad.f32",
                Attr::ReadOnly,
                s.t.res_ret,
                139,
                &[h, at, iu, mask, align],
            );
            let f = s.m.extract(b, r, 0);
            let (suffix, v) = if e.comp == 9 {
                (".f32", f)
            } else {
                let i32t = s.t.i32;
                (".i32", s.m.cast(b, Cast::Bitcast, f, i32t))
            };
            let args = [s.m.i32(n as u32), s.m.i32(0), s.m.i8(col), v];
            s.op(b, &format!("storeOutput{suffix}"), Attr::NoUnwind, s.t.void, 5, &args);
        }
    }
    s.inputs = vec![vertex_id];
    s.outputs = outputs;
    s.m.terminate(b, Term::Ret);
    Ok(MeshVertexShader {
        shader: HostShader {
            stage: HwStage::Vs,
            dxil: s.finish(),
            user_data: Vec::new(),
            samplers: Vec::new(),
            static_samplers: Vec::new(),
            root_cbvs: Vec::new(),
            root_srvs: Vec::new(),
            root_uavs: Vec::new(),
            memory: false,
            memory_writes: false,
            view_addresses: false,
            heap: false,
            heap_writes: false,
            view_marks: false,
            wave64_why: None,
        },
        stride,
    })
}
