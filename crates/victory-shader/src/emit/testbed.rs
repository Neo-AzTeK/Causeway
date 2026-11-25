//! Headless check of the translation: a hand-written GFX10 instruction
//! sequence goes through the whole pipeline to DXIL and runs on WARP (the
//! hardware adapter only when asked). Mirrors `valu_tests.rs`, which stays
//! as it is.

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

/// One GPU run at a time (see `ONE_AT_A_TIME` in `valu_tests.rs`).
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

const S_ENDPGM: u32 = 0xBF81_0000;

/// `s_mov_b32 s5, 0` (inline constant 0).
const S_MOV_S5_0: u32 = 0xBE80_0000 | 5 << 16 | 0x03 << 8 | 0x80;
/// `s_load_dwordx4 s[8:11], s[4:5], 0`.
const S_LOAD: [u32; 2] = [0x3D << 26 | 0x02 << 18 | 8 << 6 | 2, 125 << 25];
/// `buffer_store_dword v0, off, s[8:11], 0`.
const STORE_V0: [u32; 2] = [0x38 << 26 | 0x1C << 18, 0x80 << 24 | 2 << 16];

fn build(body: &[u32]) -> Result<HostShader, String> {
    let mut words = vec![S_MOV_S5_0];
    words.extend_from_slice(&S_LOAD);
    // Root constants s0..s3 to v1..v4 (`v_mov_b32`).
    for i in 0..4u32 {
        words.push(0x7E00_0000 | (i + 1) << 17 | 0x001 << 9 | i);
    }
    words.extend_from_slice(body);
    words.extend_from_slice(&STORE_V0);
    words.push(S_ENDPGM);

    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let param = |root, sgprs, user_data, slot| RootParam {
        root,
        table_offset_dwords: 0,
        sgprs,
        base_register: 0,
        user_data,
        slot,
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout: vec![
            param(RootType::Constants, 4, 0, 0),
            param(RootType::Table, 1, 4, 1),
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [1, 1, 1],
        }),
        code,
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs)
        .map_err(|x| format!("ir: {x:?}"))?;
    let st = structure::structure(&p).map_err(|x| format!("structure: {x:?}"))?;
    let res = resource::track(&p, &native.layout, native.stage, Some(&st))
        .map_err(|x| format!("resource: {x:?}"))?;
    let io = io::raise(&p, &native).map_err(|x| format!("io: {x:?}"))?;
    let lowered = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
        min_lod_clamp: false,
        view_marks: false,
    })
    .map_err(|x| format!("lower: {x:?}"))?;
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        memory: lowered.memory,
        memory_writes: lowered.memory_writes,
        view_addresses: lowered.view_addresses,
        heap: lowered.heap,
        heap_writes: lowered.heap_writes,
        view_marks: lowered.view_marks,
    };
    // shortcut: validated only when `dxil.dll` is found; otherwise the
    // device's own checks are all there is.
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy)
            .map_err(|x| format!("DXIL validation: {x:?}"))?;
    }
    Ok(shader)
}

/// Runs `words` (GFX10 instructions, one thread) with `inputs` in `v1..v4`
/// and returns `v0`. `warp` picks the software adapter, otherwise the
/// hardware one.
pub fn run_alu(words: &[u32], inputs: [u32; 4], warp: bool) -> Result<u32, String> {
    let shader = build(words)?;
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let host = Host::new(warp).map_err(|e| format!("no SM 6.6 device: {e}"))?;
    let (root, pso) = host.compute_pipeline(&shader)?;
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        ud[u.index as usize] = match (u.root, u.slot) {
            (RootType::Constants, 0) => inputs[u.dword as usize],
            (RootType::Table, 1) => 0,
            other => return Err(format!("unexpected user data {other:?}")),
        };
    }
    let out = host.dispatch(
        &root,
        &pso,
        &Dispatch {
            roots: vec![(0, Root::Constants(ud))],
            views: &[View::RawUav(1)],
            samplers: &[],
            groups: [1, 1, 1],
        },
    )?;
    Ok(out[0][0])
}
