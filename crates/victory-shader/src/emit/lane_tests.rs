//! End-to-end tests of lane masks and scalar values as each lane sees them:
//! synthetic GFX10 programs of 32 lanes, run as in `image_tests`.

use super::host::{Stride, View};
use super::image_tests::{build, check, run, translate, translate_with};
use crate::native::ResourceType;

/// Loads the raw UAV's descriptor to s[40:43] and v16 = 4 × lane.
const STORE_SETUP: [u32; 4] = [
    0xF408_0A02,
    0xFA00_0020, // s_load_dwordx4 s[40:43], s[4:5], 0x20
    0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    0x3420_0082, // v_lshlrev_b32 v16, 2, v0
];
/// Stores v8 at v16.
const STORE: [u32; 2] = [
    0xE070_1000,
    0x800A_0810, // buffer_store_dword v8, v16, s[40:43], 0 offen
];

/// The dword each lane stored.
fn stored(body: &[u32]) -> Vec<(&'static str, Vec<u32>)> {
    let words = [&STORE_SETUP[..], body, &STORE[..]].concat();
    let shader = build(&words, ResourceType::Srv, true);
    let dummy = [[0.0f32; 4]];
    run(&shader, &[View::Texture(1, 1, &dummy), View::RawUav(32)])
        .into_iter()
        .map(|(label, mut out)| (label, out.remove(0)))
        .collect()
}

#[test]
fn a_lane_mask_spilled_to_a_vgpr_lane_stays_a_mask() {
    let body = [
        0xBE97_037E, // s_mov_b32 s23, exec_lo
        0x7DA8_009C, // v_cmpx_gt_u32 exec_lo, 28, v0
        0x7D82_0083, // v_cmp_lt_u32 vcc_lo, 3, v0
        0xBEFE_0317, // s_mov_b32 exec_lo, s23
        0xD761_0005,
        0x0001_066A, // v_writelane_b32 v5, vcc_lo, 3
        0xBF06_8000, // s_cmp_eq_u32 s0, 0 (true)
        0xBF85_0004, // s_cbranch_scc1 -> the second compare
        0x7D88_0088, // v_cmp_gt_u32 vcc_lo, 8, v0
        0xD761_0005,
        0x0001_086A, // v_writelane_b32 v5, vcc_lo, 4
        0xBF82_0003, // s_branch -> the reads
        0x7D88_0098, // v_cmp_gt_u32 vcc_lo, 24, v0
        0xD761_0005,
        0x0001_086A, // v_writelane_b32 v5, vcc_lo, 4
        0xD760_0006,
        0x0001_0705, // v_readlane_b32 s6, v5, 3
        0xD760_0008,
        0x0001_0905, // v_readlane_b32 s8, v5, 4
        0xD501_0008,
        0x0019_0280, // v_cndmask_b32 v8, 0, 1, s6
        0xD501_0009,
        0x0021_0480, // v_cndmask_b32 v9, 0, 2, s8
        0x3810_1109, // v_or_b32 v8, v9, v8
    ];
    for (label, got) in stored(&body) {
        let want: Vec<u32> = (0..32)
            .map(|l| u32::from((4..28).contains(&l)) | (2 * u32::from(l < 24)))
            .collect();
        assert_eq!(got, want, "{label}");
    }
}

/// s9 holds a descriptor's dword; lanes below 16 set it to 100 and v5 to -1
/// (0 in the others).
const REUSED: [u32; 12] = [
    0xF408_0203,
    0xFA00_0000, // s_load_dwordx4 s[8:11], s[6:7], 0x0
    0x7E0A_0280, // v_mov_b32 v5, 0
    0x7E10_0280, // v_mov_b32 v8, 0
    0x7D88_0090, // v_cmp_gt_u32 vcc_lo, 16, v0
    0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
    0xBF88_0003, // s_cbranch_execz -> the restore
    0xBE89_03FF,
    0x0000_0064, // s_mov_b32 s9, 100
    0x7E0A_02C1, // v_mov_b32 v5, -1
    0xBEFE_0317, // s_mov_b32 exec_lo, s23
    0x7D84_0AC1, // v_cmp_eq_u32 vcc_lo, -1, v5
];
const USE: u32 = 0x4A10_0009; // v_add_nc_u32 v8, s9, v0

#[test]
fn a_register_reused_on_another_path_is_read_where_a_flag_allows() {
    let want: Vec<u32> = (0..32).map(|l| if l < 16 { 100 + l } else { 0 }).collect();
    // The flag guards a branch, or EXEC alone.
    let branch = [
        0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
        0xBF88_0001, // s_cbranch_execz -> the restore
        USE,
        0xBEFE_0317, // s_mov_b32 exec_lo, s23
    ];
    let guard = [branch[0], USE, branch[3]];
    for read in [&branch[..], &guard[..]] {
        for (label, got) in stored(&[&REUSED[..], read].concat()) {
            assert_eq!(got, want, "{label}");
        }
    }
}

#[test]
fn a_register_reused_on_another_path_is_refused_where_lanes_from_it_read() {
    let after = |read: &[u32]| {
        let words = [&STORE_SETUP[..], &REUSED[..], read, &STORE[..]].concat();
        translate(&words, ResourceType::Srv)
    };
    // Unguarded; guarded by the other lanes' flag; read across lanes.
    assert!(after(&[USE]).is_err());
    assert!(
        after(&[
            0x7D8A_0AC1, // v_cmp_ne_u32 vcc_lo, -1, v5
            0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
            USE,
            0xBEFE_0317, // s_mov_b32 exec_lo, s23
        ])
        .is_err()
    );
    assert!(
        after(&[
            0x7E0C_0209, // v_mov_b32 v6, s9
            0x7E14_0506, // v_readfirstlane_b32 s10, v6
            0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
            0x4A10_000A, // v_add_nc_u32 v8, s10, v0
            0xBEFE_0317, // s_mov_b32 exec_lo, s23
        ])
        .is_err()
    );
}

#[test]
fn the_record_count_of_a_scalar_loaded_buffer_is_the_hosts() {
    // The last element's dword: element s10 - 1 (word 2 of the descriptor)
    // times the stride; then plus the count itself.
    let body = |scale: u32| {
        [
            0xF408_0202,
            0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0x810C_C10A, // s_add_i32 s12, s10, -1
            0x930C_FF0C,
            scale, // s_mul_i32 s12, s12, scale
            0xF420_0344,
            0x1800_0000, // s_buffer_load_dword s13, s[8:11], s12
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0x810D_0A0D, // s_add_i32 s13, s13, s10
            0x7E10_020D, // v_mov_b32 v8, s13
        ]
    };
    let data: Vec<u32> = (0..24).map(|n| 1000 * n).collect();
    let run_with = |body: &[u32], stride, view| {
        let words = [&STORE_SETUP[..], body, &STORE[..]].concat();
        let shader = translate_with(&words, ResourceType::Srv, 4, stride).unwrap();
        check(&shader);
        let view = View::Buffer {
            data: &data,
            stride: view,
        };
        run(&shader, &[view, View::RawUav(32)])
    };
    // 6 structures of 16 bytes; 96 raw bytes, the last dword's offset 92
    // (the load drops the low bits of 95).
    for (label, out) in run_with(&body(16), 16, Stride::Structured(16)) {
        assert_eq!(out[0], vec![20_000 + 6; 32], "{label}");
    }
    for (label, out) in run_with(&body(1)[..], 0, Stride::Raw) {
        assert_eq!(out[0][0], 23_000 + 96, "{label}");
    }
}
