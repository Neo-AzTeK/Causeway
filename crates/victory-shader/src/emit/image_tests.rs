//! End-to-end tests of image instructions: synthetic GFX10 programs lowered
//! to DXIL, validated by `dxil.dll` when present, and run on the host GPU
//! and WARP.

use windows::Win32::Graphics::Direct3D12::*;

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

/// `s[4:5]` and `s[6:7]`: the tables of views and of samplers.
const TABLES: [u32; 4] = [
    0xBE84_0300, // s_mov_b32 s4, s0
    0xBE85_0380, // s_mov_b32 s5, 0
    0xBE86_0301, // s_mov_b32 s6, s1
    0xBE87_0380, // s_mov_b32 s7, 0
];
const S_ENDPGM: u32 = 0xBF81_0000;

/// Lowers a wave32 compute program of 32 threads with the table of views at
/// s0 (a texture of kind `image` at dword 0, a raw UAV at dword 8) and the
/// table of samplers at s1; or why it does not translate.
pub(super) fn translate(body: &[u32], image: ResourceType) -> Result<HostShader, ShaderError> {
    translate_with(body, image, 8, 0, false)
}

/// [`translate`] with another view at dword 0: the dwords of its descriptor
/// and, of a structured buffer, the stride.
pub(super) fn translate_with(
    body: &[u32],
    image: ResourceType,
    dwords: u8,
    stride: u32,
    wave64: bool,
) -> Result<HostShader, ShaderError> {
    translate_group(body, (image, dwords, stride), wave64, 32, 0)
}

/// [`translate_with`] for a group of `threads` with `lds` blocks of 512
/// bytes; `view` is its kind, dwords and stride.
pub(super) fn translate_group(
    body: &[u32],
    view: (ResourceType, u8, u32),
    wave64: bool,
    threads: u32,
    lds: u32,
) -> Result<HostShader, ShaderError> {
    let (image, dwords, stride) = view;
    let table = |user_data: u8| RootParam {
        root: RootType::Table,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot: user_data,
    };
    let range = |kind, table_offset_dwords, dwords, stride, user_data| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: 0,
        stride,
        dwords,
        user_data,
        slot: user_data,
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs: 32,
        layout: vec![table(0), table(1)],
        used: vec![
            range(image, 0, dwords, stride, 0),
            range(ResourceType::Uav, 8, 4, 0, 0),
            range(ResourceType::Sampler, 0, 4, 0, 1),
        ],
        io: StageIo::Cs(ComputeIo {
            rsrc2: lds << 15 | 2 << 1,
            threads: [threads, 1, 1],
        }),
        code: TABLES
            .iter()
            .chain(body)
            .chain(&[S_ENDPGM])
            .flat_map(|w| w.to_le_bytes())
            .collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, &native).expect("io::raise");
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
        view_marks: false,
    })?;
    Ok(HostShader {
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
    })
}

/// A program that translates, `validate`d if asked (see [`translate`]).
pub(super) fn build(body: &[u32], image: ResourceType, validate: bool) -> HostShader {
    let shader = translate(body, image).expect("translate");
    if validate {
        check(&shader);
    }
    shader
}

/// Has `dxil.dll` validate `shader`.
pub(super) fn check(shader: &HostShader) {
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    } else {
        eprintln!("no dxil.dll; validation skipped");
    }
}

/// Runs `shader` with `views` at heap indices 0.. on the hardware GPU and on
/// WARP (each skipped without a device) and returns the UAVs' dwords per one
/// that ran.
pub(super) fn run(shader: &HostShader, views: &[View]) -> Vec<(&'static str, Vec<Vec<u32>>)> {
    let out = run_on(shader, views, &[("hardware", false), ("warp", true)]);
    assert!(!out.is_empty());
    out
}

/// [`run`] on these adapters (a name, and whether it is WARP).
pub(super) fn run_on(
    shader: &HostShader,
    views: &[View],
    adapters: &[(&'static str, bool)],
) -> Vec<(&'static str, Vec<Vec<u32>>)> {
    let samplers = [D3D12_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MaxAnisotropy: 1,
        MaxLOD: f32::MAX,
        ..Default::default()
    }];
    let mut out = Vec::new();
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for &(label, warp) in adapters {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = match host.compute_pipeline(shader) {
            Ok(p) => p,
            Err(e) => panic!("{label}: compute_pipeline failed: {e}"),
        };
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let words = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views,
                    samplers: &samplers,
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, words));
    }
    out
}

#[test]
fn a_biased_sample_with_16_bit_coordinates_takes_an_f16_bias() {
    // Lanes of a quad are 1.0 apart in u, so a 4x4 texture is sampled at
    // its smallest level or near it, less the bias: the VGPR before the
    // coordinates' holds -4.0 as an f16. (The validator allows a biased
    // sample in pixel shaders only; both adapters run it.)
    let body = [
        0xF408_0203,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[6:7], 0x0
        0xF40C_0302,
        0xFA00_0000, // s_load_dwordx8 s[12:19], s[4:5], 0x0
        0xF408_0A02,
        0xFA00_0020, // s_load_dwordx4 s[40:43], s[4:5], 0x20
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x3420_0082, // v_lshlrev_b32 v16, 2, v0
        0x3606_0081, // v_and_b32 v3, 1, v0
        0x7E06_0D03, // v_cvt_f32_u32 v3, v3
        0x7E06_1503, // v_cvt_f16_f32 v3, v3 (v = 0 in the high half)
        0x7E04_02FF,
        0x0000_C400, // v_mov_b32 v2, 0xc400
        0xF094_0108,
        0x4043_0C02, // image_sample_b v12, v[2:3], s[12:19], s[8:11] dmask:0x1 a16
        0xBF8C_3F70, // s_waitcnt vmcnt(0)
        0xE070_1000,
        0x800A_0C10, // buffer_store_dword v12, v16, s[40:43], 0 offen
    ];
    let shader = build(&body, ResourceType::Srv, false);
    let levels = [10.0f32, 20.0, 30.0].map(|r| [r, 0.0, 0.0, 1.0]);
    for (label, out) in run(&shader, &[View::Mips(&levels), View::RawUav(32)]) {
        let red: Vec<f32> = out[0].iter().map(|&x| f32::from_bits(x)).collect();
        assert_eq!(red, vec![10.0f32; 32], "{label}");
    }
}

#[test]
fn a_64_bit_image_atomic_updates_the_whole_texel() {
    // Lanes below 24 write to texel (n & 3, n >> 3) the maximum of (high
    // dword: 1 if bit 2 of n is clear, low dword: n), which a comparison of
    // either dword alone gets wrong.
    let body = [
        0xF40C_0302,
        0xFA00_0000, // s_load_dwordx8 s[12:19], s[4:5], 0x0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x3604_0083, // v_and_b32 v2, 3, v0
        0x2C06_0083, // v_lshrrev_b32 v3, 3, v0
        0x2C0A_0082, // v_lshrrev_b32 v5, 2, v0
        0x360A_0A81, // v_and_b32 v5, 1, v5
        0x3A0A_0A81, // v_xor_b32 v5, 1, v5
        0x7E08_0300, // v_mov_b32 v4, v0
        0x7DA8_0098, // v_cmpx_gt_u32 exec_lo, 24, v0
        0xF05C_1308,
        0x0003_0402, // image_atomic_umax v[4:5], v[2:3], s[12:19] dmask:0x3 unorm
    ];
    let shader = build(&body, ResourceType::Uav, true);
    for (label, out) in run(&shader, &[View::Uint2TextureUav(4, 4), View::RawUav(32)]) {
        // Texel (x, y): lanes 8y + x (high dword 1) and 8y + 4 + x (0).
        let want: Vec<u32> = (0..16)
            .flat_map(|t| {
                let (x, y) = (t % 4, t / 4);
                if y < 3 { [8 * y + x, 1] } else { [0, 0] }
            })
            .collect();
        assert_eq!(out[0], want, "{label}");
    }
}

#[test]
fn a_64_bit_image_atomic_returns_the_old_value() {
    // Lanes below 24, each on texel (n & 3, n >> 2), exchange twice; every
    // lane stores its v5, the high dword.
    let body = [
        0xF40C_0302,
        0xFA00_0000, // s_load_dwordx8 s[12:19], s[4:5], 0x0
        0xF408_0A02,
        0xFA00_0020, // s_load_dwordx4 s[40:43], s[4:5], 0x20
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x3420_0082, // v_lshlrev_b32 v16, 2, v0
        0x3604_0083, // v_and_b32 v2, 3, v0
        0x2C06_0082, // v_lshrrev_b32 v3, 2, v0
        0x7E08_0300, // v_mov_b32 v4, v0
        0x4A0A_00FF,
        0x0000_0100, // v_add_nc_u32 v5, 0x100, v0
        0xBE97_037E, // s_mov_b32 s23, exec_lo
        0x7DA8_0098, // v_cmpx_gt_u32 exec_lo, 24, v0
        0xF03C_3308,
        0x0003_0402, // image_atomic_swap v[4:5], v[2:3], s[12:19] dmask:0x3 unorm glc
        0xBF8C_3F70, // s_waitcnt vmcnt(0)
        0x7E0C_0305, // v_mov_b32 v6, v5
        0xF03C_3308,
        0x0003_0402, // image_atomic_swap v[4:5], v[2:3], s[12:19] dmask:0x3 unorm glc
        0xBF8C_3F70, // s_waitcnt vmcnt(0)
        0xBEFE_0317, // s_mov_b32 exec_lo, s23
        0xE070_1000,
        0x800A_0510, // buffer_store_dword v5, v16, s[40:43], 0 offen
    ];
    let shader = build(&body, ResourceType::Uav, true);
    for (label, out) in run(&shader, &[View::Uint2TextureUav(4, 8), View::RawUav(32)]) {
        // The first exchange returns the texel's zeros, which the second
        // puts back; it returns the lane's own value. The other lanes'
        // registers keep theirs.
        assert_eq!(out[0], vec![0; 64], "{label}");
        let want: Vec<u32> = (0..32).map(|n| 0x100 + n).collect();
        assert_eq!(out[1], want, "{label}");
    }
}

#[test]
fn a_64_bit_compare_and_swap_or_subtraction_carries_across_the_dwords() {
    // Each lane on its own texel: even lanes swap {0x100 + n : n} into the
    // zeros, odd lanes compare with {1 : 0} and do not; then all subtract
    // n + 1.
    let body = [
        0xF40C_0302,
        0xFA00_0000, // s_load_dwordx8 s[12:19], s[4:5], 0x0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x3604_0083, // v_and_b32 v2, 3, v0
        0x2C06_0082, // v_lshrrev_b32 v3, 2, v0
        0x7E08_0300, // v_mov_b32 v4, v0
        0x4A0A_00FF,
        0x0000_0100, // v_add_nc_u32 v5, 0x100, v0
        0x7E0C_0280, // v_mov_b32 v6, 0
        0x360E_0081, // v_and_b32 v7, 1, v0
        0x4A14_0081, // v_add_nc_u32 v10, 1, v0
        0x7E16_0280, // v_mov_b32 v11, 0
        0xF040_1F08,
        0x0003_0402, // image_atomic_cmpswap v[4:7], v[2:3], s[12:19] dmask:0xf unorm
        0xF048_1308,
        0x0003_0A02, // image_atomic_sub v[10:11], v[2:3], s[12:19] dmask:0x3 unorm
    ];
    let shader = build(&body, ResourceType::Uav, true);
    for (label, out) in run(&shader, &[View::Uint2TextureUav(4, 8), View::RawUav(32)]) {
        let want: Vec<u32> = (0..32u32)
            .flat_map(|n| {
                let old = if n & 1 == 0 {
                    (u64::from(0x100 + n) << 32) | u64::from(n)
                } else {
                    0
                };
                let x = old.wrapping_sub(u64::from(n) + 1);
                [x as u32, (x >> 32) as u32]
            })
            .collect();
        assert_eq!(out[0], want, "{label}");
    }
}
