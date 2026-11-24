//! End-to-end tests of lane masks and scalar values as each lane sees them:
//! synthetic GFX10 programs of 32 lanes, run as in `image_tests`.

use super::host::View;
use super::image_tests::{build, run};
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
