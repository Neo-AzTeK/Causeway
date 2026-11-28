//! End-to-end tests of lane masks and scalar values as each lane sees them:
//! synthetic GFX10 programs of 32 lanes, run as in `image_tests`.

use super::host::{Host, Stride, View};
use super::image_tests::{
    build, check, run, run_on, translate, translate_group, translate_halves, translate_together,
    translate_with,
};
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

/// s11 holds a descriptor's dword (word 3: words 0 and 1 are the host's view
/// addresses, which read as real values); lanes below 16 set it to 100 and v5 to -1
/// (0 in the others).
const REUSED: [u32; 12] = [
    0xF408_0203,
    0xFA00_0000, // s_load_dwordx4 s[8:11], s[6:7], 0x0
    0x7E0A_0280, // v_mov_b32 v5, 0
    0x7E10_0280, // v_mov_b32 v8, 0
    0x7D88_0090, // v_cmp_gt_u32 vcc_lo, 16, v0
    0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
    0xBF88_0003, // s_cbranch_execz -> the restore
    0xBE8B_03FF,
    0x0000_0064, // s_mov_b32 s11, 100
    0x7E0A_02C1, // v_mov_b32 v5, -1
    0xBEFE_0317, // s_mov_b32 exec_lo, s23
    0x7D84_0AC1, // v_cmp_eq_u32 vcc_lo, -1, v5
];
const USE: u32 = 0x4A10_000B; // v_add_nc_u32 v8, s11, v0

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
            0x7E0C_020B, // v_mov_b32 v6, s11
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
        let shader = translate_with(&words, ResourceType::Srv, 4, stride, false).unwrap();
        check(&shader);
        let view = View::Buffer {
            data: &data,
            stride: view,
        };
        run(&shader, &[view, View::RawUav(32)])
    };
    // With a stride the code names (VIctory's rule: the view has it): 96
    // bytes are 6 records of 16, the last at byte 80, dword 20.
    for (label, out) in run_with(&body(16)[..], 16, Stride::Structured(16)) {
        assert_eq!(out[0][0], 20_000 + 6, "{label}");
    }
    // 96 raw bytes, the last dword's offset 92 (the load drops the low
    // bits of 95).
    for (label, out) in run_with(&body(1)[..], 0, Stride::Raw) {
        assert_eq!(out[0][0], 23_000 + 96, "{label}");
    }
}

#[test]
fn elements_and_record_counts_are_read_through_the_raw_host_view() {
    // A lane's element of a structured buffer, plus the record count (word 2
    // of the descriptor). The host views it as raw.
    let body = [
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xE030_2004,
        0x8002_0800, // buffer_load_dword v8, v0, s[8:11], 0 idxen offset:4
        0xBF8C_0000, // s_waitcnt 0
        0x4A10_100A, // v_add_nc_u32 v8, s10, v8
    ];
    let data: Vec<u32> = (0..24).map(|n| 1000 * n).collect();
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    let shader = translate_with(&words, ResourceType::Srv, 4, 16, false).unwrap();
    check(&shader);
    let view = View::Buffer {
        data: &data,
        stride: Stride::Raw,
    };
    for (label, out) in run(&shader, &[view, View::RawUav(32)]) {
        for (lane, &x) in out[0].iter().take(8).enumerate() {
            let element = data.get(4 * lane + 1).copied().unwrap_or(0);
            assert_eq!(x, element + 6, "{label} lane {lane}");
        }
    }
}

#[test]
fn a_second_use_of_a_reused_register_is_told_from_the_first_paths() {
    // The same flag and register again: lanes below 8 set both anew.
    let body = [
        &REUSED[..],
        &[
            0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
            0xBF88_0001, // s_cbranch_execz -> the restore
            USE,
            0xBEFE_0317, // s_mov_b32 exec_lo, s23
            0x7E0A_0280, // v_mov_b32 v5, 0
            0x7D88_0088, // v_cmp_gt_u32 vcc_lo, 8, v0
            0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
            0xBF88_0003, // s_cbranch_execz -> the restore
            0xBE8B_03FF,
            0x0000_00C8, // s_mov_b32 s11, 200
            0x7E0A_02C1, // v_mov_b32 v5, -1
            0xBEFE_0317, // s_mov_b32 exec_lo, s23
            0x7D84_0AC1, // v_cmp_eq_u32 vcc_lo, -1, v5
            0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
            0xBF88_0001, // s_cbranch_execz -> the restore
            USE,
            0xBEFE_0317, // s_mov_b32 exec_lo, s23
        ][..],
    ]
    .concat();
    let want: Vec<u32> = (0..32)
        .map(|l| match l {
            0..8 => 200 + l,
            8..16 => 100 + l,
            _ => 0,
        })
        .collect();
    for (label, got) in stored(&body) {
        assert_eq!(got, want, "{label}");
    }
}

#[test]
fn a_descriptor_loaded_again_for_its_size_has_the_first_loads_kind() {
    // Lane n samples texture n & 1 in one waterfall loop and reads its
    // width from the descriptor, loaded in another (lanes below 28: the
    // others' EXEC bits are clear).
    let body = [
        0xF408_0A02,
        0xFA00_0040, // s_load_dwordx4 s[40:43], s[4:5], 0x40
        0xF408_0203,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[6:7], 0x0
        0x3420_0082, // v_lshlrev_b32 v16, 2, v0
        0x7E04_0280, // v_mov_b32 v2, 0
        0x7E06_0280, // v_mov_b32 v3, 0
        0x360E_0081, // v_and_b32 v7, 1, v0
        0x340E_0E85, // v_lshlrev_b32 v7, 5, v7
        0xBE98_037E, // s_mov_b32 s24, exec_lo
        0x7DA8_009C, // v_cmpx_gt_u32 exec_lo, 28, v0
        0xBE97_037E, // s_mov_b32 s23, exec_lo
        0xBE95_037E, // s_mov_b32 s21, exec_lo
        0x7E28_0507, // loop: v_readfirstlane_b32 s20, v7
        0x7DA4_0E14, // v_cmpx_eq_u32 exec_lo, s20, v7
        0xF40C_0302,
        0x2800_0000, // s_load_dwordx8 s[12:19], s[4:5], s20
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xF09C_0F08,
        0x0043_0C02, // image_sample_lz v[12:15], v[2:3], s[12:19], s[8:11] dmask:0xf
        0xBF8C_3F70, // s_waitcnt vmcnt(0)
        0xBE95_4715, // s_andn2_wrexec_b32 s21, s21
        0xBF89_FFF6, // s_cbranch_execnz -> loop
        0xBEFE_0317, // s_mov_b32 exec_lo, s23
        0xBE95_037E, // s_mov_b32 s21, exec_lo
        0x7E28_0507, // loop: v_readfirstlane_b32 s20, v7
        0x7DA4_0E14, // v_cmpx_eq_u32 exec_lo, s20, v7
        0xF40C_0302,
        0x2800_0000, // s_load_dwordx8 s[12:19], s[4:5], s20
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xD54E_0008,
        0x0278_1A0E, // v_alignbit_b32 v8, s14, s13, 30
        0xBE95_4715, // s_andn2_wrexec_b32 s21, s21
        0xBF89_FFF7, // s_cbranch_execnz -> loop
        0xBEFE_0318, // s_mov_b32 exec_lo, s24
        0x3610_10FF,
        0x0000_3FFF, // v_and_b32 v8, 0x3fff, v8
    ];
    let words = [&body[..], &STORE[..]].concat();
    let shader = build(&words, ResourceType::Srv, true);
    let (wide, narrow) = ([[0.0f32; 4]; 5], [[0.0f32; 4]; 3]);
    let views = [
        View::Texture(5, 1, &wide),
        View::Texture(3, 1, &narrow),
        View::RawUav(32),
    ];
    let want: Vec<u32> = (0..32)
        .map(|l| if l < 28 { [4, 2][l % 2] } else { 0 })
        .collect();
    for (label, out) in run(&shader, &views) {
        assert_eq!(out[0], want, "{label}");
    }
}

/// A mask is counted where it is written: a lane whose bit depends on the
/// reused register votes even if it branches past the count.
#[test]
fn a_mask_counted_past_a_branch_on_a_reused_register_is_refused() {
    let readers: [&[u32]; 5] = [
        // s_bcnt1_i32_b32 s10, s24; v_mov_b32 v8, s10
        &[0xBE8A_0F18, 0x7E10_020A],
        // s_ff1_i32_b32 s10, s24; v_mov_b32 v8, s10
        &[0xBE8A_1318, 0x7E10_020A],
        // s_cmp_eq_u32 s24, 0; s_cselect_b32 s10, 1, 0; v_mov_b32 v8, s10
        &[0xBF06_8018, 0x850A_8081, 0x7E10_020A],
        // s_cmp_lg_u32 s24, 0; s_cselect_b32 s10, 1, 0; v_mov_b32 v8, s10
        &[0xBF07_8018, 0x850A_8081, 0x7E10_020A],
        // v_mbcnt_lo_u32_b32 v8, s24, 0
        &[0xD765_0008, 0x0001_0018],
    ];
    for read in readers {
        let skip = read.len() as u32;
        let body = [
            &REUSED[..],
            &[
                0xD4C4_0018,
                0x0002_000B, // v_cmp_gt_u32 s24, s11, v0
                0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
                0xBF88_0000 | skip, // s_cbranch_execz -> the restore
            ],
            read,
            &[0xBEFE_0317], // s_mov_b32 exec_lo, s23
        ]
        .concat();
        let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
        assert!(translate(&words, ResourceType::Srv).is_err(), "{read:x?}");
    }
}

#[test]
fn a_register_reused_on_another_path_is_told_by_its_flag_in_wave64() {
    let reused = [
        &REUSED[..5],
        &[
            0xBE96_246A, // s_and_saveexec_b64 s[22:23], vcc
            0xBF88_0003, // s_cbranch_execz -> the restore
        ],
        &REUSED[7..10],
        &[
            0xBEFE_0416, // s_mov_b64 exec, s[22:23]
            REUSED[11],
        ],
    ]
    .concat();
    let after = |read: &[u32]| {
        let words = [&STORE_SETUP[..], &reused, read, &STORE[..]].concat();
        translate_with(&words, ResourceType::Srv, 8, 0, true)
    };
    let guarded = after(&[
        0xBE96_246A, // s_and_saveexec_b64 s[22:23], vcc
        0xBF88_0001, // s_cbranch_execz -> the restore
        USE,
        0xBEFE_0416, // s_mov_b64 exec, s[22:23]
    ]);
    check(&guarded.unwrap());
    assert!(after(&[USE]).is_err());
}

/// v8 = 0, then three passes of a loop that adds s11 to it where v5 is -1;
/// `first` sets the two before the loop, `next` after each pass.
fn looped(first: &[u32], next: &[u32]) -> Vec<u32> {
    let pass = [
        &[
            0x7D84_0AC1, // v_cmp_eq_u32 vcc_lo, -1, v5
            0xBE97_3C6A, // s_and_saveexec_b32 s23, vcc_lo
            0xBF88_0001, // s_cbranch_execz -> the restore
            0x4A10_100B, // v_add_nc_u32 v8, s11, v8
            0xBEFE_0317, // s_mov_b32 exec_lo, s23
        ][..],
        next,
        &[
            0x8114_8114, // s_add_i32 s20, s20, 1
            0xBF0A_8314, // s_cmp_lt_u32 s20, 3
        ],
    ]
    .concat();
    // s_cbranch_scc1 -> the pass
    let back = 0xBF85_0000 | ((u32::MAX - pass.len() as u32) & 0xFFFF);
    [
        &[
            0x7E10_0280, // v_mov_b32 v8, 0
            0xBE94_0380, // s_mov_b32 s20, 0
        ][..],
        first,
        &pass,
        &[back],
    ]
    .concat()
}

#[test]
fn a_register_reused_across_a_back_edge_is_read_where_a_flag_allows() {
    const LOAD: [u32; 2] = [0xF408_0203, 0xFA00_0000]; // s_load_dwordx4 s[8:11], s[6:7], 0x0
    const OFF: u32 = 0x7E0A_0280; // v_mov_b32 v5, 0
    const ON: u32 = 0x7E0A_02C1; // v_mov_b32 v5, -1
    const TEN: u32 = 0xBE8B_038A; // s_mov_b32 s11, 10
    // The dword comes in, the number over the back edge.
    let body = looped(&[LOAD[0], LOAD[1], OFF], &[TEN, ON]);
    for (label, got) in stored(&body) {
        assert_eq!(got, [20; 32], "{label}");
    }
    // The other way round.
    let body = looped(&[TEN, ON], &[LOAD[0], LOAD[1], OFF]);
    for (label, got) in stored(&body) {
        assert_eq!(got, [10; 32], "{label}");
    }
    // The flag stays.
    let body = looped(&[TEN, ON], &LOAD);
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    assert!(translate(&words, ResourceType::Srv).is_err());
}

#[test]
fn a_typed_buffer_is_not_sized_as_a_raw_one() {
    const TYPED: [u32; 2] = [
        0xE000_2000,
        0x8002_0900, // buffer_load_format_x v9, v0, s[8:11], 0 idxen
    ];
    const SCALAR: [u32; 2] = [
        0xF420_0344,
        0xFA00_0000, // s_buffer_load_dword s13, s[8:11], 0
    ];
    let after = |uses: [[u32; 2]; 2]| {
        let words = [
            &STORE_SETUP[..],
            &[
                0xF408_0202,
                0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
            ],
            &uses.concat(),
            &[0x7E10_020A], // v_mov_b32 v8, s10
            &STORE[..],
        ]
        .concat();
        translate_with(&words, ResourceType::Srv, 4, 0, false)
    };
    assert!(after([SCALAR, TYPED]).is_err());
    let shader = after([TYPED, SCALAR]).unwrap();
    let data = [0u32; 24];
    let view = View::Buffer {
        data: &data,
        stride: Stride::Typed,
    };
    for (label, out) in run(&shader, &[view, View::RawUav(32)]) {
        assert_eq!(out[0], vec![6; 32], "{label}");
    }
}

/// Titles read raw views by scalar loads the code names a stride for.
#[test]
fn a_scalar_load_reads_a_raw_view_whatever_the_stride_named() {
    let body = [
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0xF420_0344,
        0xFA00_0054, // s_buffer_load_dword s13, s[8:11], 0x54
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x7E10_020D, // v_mov_b32 v8, s13
    ];
    let data: Vec<u32> = (0..32).map(|n| 1000 * n).collect();
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    let shader = translate_with(&words, ResourceType::Srv, 4, 64, false).unwrap();
    check(&shader);
    let view = View::Buffer { data: &data, stride: Stride::Raw };
    for (label, out) in run(&shader, &[view, View::RawUav(32)]) {
        assert_eq!(out[0], vec![21_000; 32], "{label}");
    }
}

#[test]
fn a_group_of_one_wave64_shares_lds_on_hosts_with_waves_of_32() {
    // Lane l writes l + 1 and reads lane 63 - l's dword, which the console
    // needs no barrier for.
    let body = [
        0x4A02_0081, // v_add_nc_u32 v1, 1, v0
        0xD834_0000,
        0x0000_0110, // ds_write_b32 v16, v1
        0x4C22_00BF, // v_sub_nc_u32 v17, 63, v0
        0x3422_2282, // v_lshlrev_b32 v17, 2, v17
        0xD8D8_0000,
        0x0800_0011, // ds_read_b32 v8, v17
        0xBF8C_0000, // s_waitcnt 0
    ];
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    let shader = translate_group(&words, (ResourceType::Srv, 8, 0), true, 64, 1).unwrap();
    check(&shader);
    let dummy = [[0.0f32; 4]];
    for (label, out) in run(&shader, &[View::Texture(1, 1, &dummy), View::RawUav(64)]) {
        let want: Vec<u32> = (0..64).map(|l| 64 - l).collect();
        assert_eq!(out[0], want, "{label}");
    }
}

#[test]
fn wave64_lanes_that_stay_inside_32_run_on_hosts_with_waves_of_32() {
    // Each lane takes its neighbour's id, within a pair.
    let swap = [
        0xD8D4_041F,
        0x0800_0000, // ds_swizzle_b32 v8, v0 offset: and 31, or 0, xor 1
        0xBF8C_0000, // s_waitcnt 0
    ];
    let words = [&STORE_SETUP[..], &swap, &STORE[..]].concat();
    let shader = translate_group(&words, (ResourceType::Srv, 8, 0), true, 64, 0).unwrap();
    check(&shader);
    // WARP runs no fixed wave size of 32.
    if !Host::new(false).is_ok_and(|h| h.has_waves_of(32)) {
        return;
    }
    let dummy = [[0.0f32; 4]];
    let views = [View::Texture(1, 1, &dummy), View::RawUav(64)];
    for (label, out) in run_on(&shader, &views, &[("hardware", false)]) {
        let want: Vec<u32> = (0..64).map(|l| l ^ 1).collect();
        assert_eq!(out[0], want, "{label}");
    }
}

/// Runs `body` as one console wave of 64 whose first `threads` lanes exist,
/// on the hardware if its waves are 32 wide: what each thread stored.
fn on_waves_of_32(body: &[u32], threads: u32) -> Option<Vec<u32>> {
    let words = [&STORE_SETUP[..], body, &STORE[..]].concat();
    let shader = translate_group(&words, (ResourceType::Srv, 8, 0), true, threads, 0).unwrap();
    check(&shader);
    // WARP runs no fixed wave size of 32.
    if !Host::new(false).is_ok_and(|h| h.has_waves_of(32)) {
        return None;
    }
    let dummy = [[0.0f32; 4]];
    let views = [View::Texture(1, 1, &dummy), View::RawUav(threads.max(64))];
    let mut out = run_on(&shader, &views, &[("hardware", false)]);
    Some(out.remove(0).1.remove(0)[..threads as usize].to_vec())
}

/// A group whose last console wave has no upper half: that half gives the
/// operation's identity, not what its exchange slot happens to hold. A minimum
/// shows it where unwritten group memory reads as zero.
#[test]
fn a_wave64_min_ignores_a_host_wave_that_does_not_exist() {
    let mut body = vec![
        0xBE96_25C1, // s_or_saveexec_b64 s[22:23], -1
        0xD501_0001,
        0x005A_00C1 | 0x100 << 9, // v_cndmask_b32 v1, -1, v0, s[22:23]
        0x7E10_02C1,              // v_mov_b32 v8, -1
    ];
    for k in 0..64 {
        body.extend([
            0xD760_0004,
            0x101 | (128 + k) << 9, // v_readlane_b32 s4, v1, k
            0x2610_1004,            // v_min_u32 v8, s4, v8
        ]);
    }
    body.push(0xBEFE_0416); // s_mov_b64 exec, s[22:23]
    // 96: one whole console wave and one with its lower half only.
    if let Some(got) = on_waves_of_32(&body, 96) {
        let want: Vec<u32> = (0..96).map(|l| l / 64 * 64).collect();
        assert_eq!(got, want);
    }
}

#[test]
fn wave64_lane_numbers_and_ballots_run_on_two_waves_of_32() {
    let lane = [
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0008,
        0x0002_02C1, // v_mbcnt_hi_u32_b32 v8, -1, v1
    ];
    // s[24:25] = lanes below 40.
    let ballot = [0xD4C4_0018, 0x0002_00A8];
    let count = [
        0xBE8A_1018, // s_bcnt1_i32_b64 s10, s[24:25]
        0x7E10_020A, // v_mov_b32 v8, s10
    ];
    let below = [
        0xD765_0001,
        0x0001_0018, // v_mbcnt_lo_u32_b32 v1, s24, 0
        0xD766_0008,
        0x0002_0219, // v_mbcnt_hi_u32_b32 v8, s25, v1
    ];
    // s[24:25] = the console wave's lanes numbered below 40 (v8, from `lane`).
    let by_lane = [&lane[..], &[0xD4C4_0018, 0x0002_10A8], &count].concat();
    // Lanes 48 and up have their EXEC bit clear. 96 threads: the second console
    // wave has only its lower half.
    for threads in [64u32, 48, 96] {
        // `ballot` compares thread ids: none of the second wave's are below 40.
        let ids = |f: &dyn Fn(u32) -> u32| (0..threads).map(f).collect::<Vec<u32>>();
        let want: [(&[u32], Vec<u32>); 4] = [
            (&lane, ids(&|t| t % 64)),
            (&[&ballot[..], &count].concat(), ids(&|t| if t < 64 { 40 } else { 0 })),
            (
                &[&ballot[..], &below].concat(),
                ids(&|t| if t < 64 { t.min(40) } else { 0 }),
            ),
            // 64 lanes exist in wave 0, 32 in the second wave of 96 threads.
            (&by_lane, ids(&|t| (threads - t / 64 * 64).min(40))),
        ];
        for (body, want) in want {
            if let Some(got) = on_waves_of_32(body, threads) {
                assert_eq!(got, want, "{threads} threads, {body:x?}");
            }
        }
    }
}

#[test]
fn a_wave64_sum_runs_on_two_waves_of_32() {
    // The lanes' v0 added through lane reads inside a whole-wave section.
    let mut body = vec![
        0xBE96_25C1, // s_or_saveexec_b64 s[22:23], -1
        0xD501_0001,
        0x005A_0080, // v_cndmask_b32 v1, 0, v0, s[22:23]
        0x7E10_0280, // v_mov_b32 v8, 0
    ];
    for k in 0..64 {
        body.extend([
            0xD760_0004,
            0x101 | (128 + k) << 9, // v_readlane_b32 s4, v1, k
            0x4A10_1004,            // v_add_nc_u32 v8, s4, v8
        ]);
    }
    body.push(0xBEFE_0416); // s_mov_b64 exec, s[22:23]
    if let Some(got) = on_waves_of_32(&body, 64) {
        assert_eq!(got, vec![2016; 64]);
    }
}

#[test]
fn a_wave64_16_bit_sum_runs_on_two_waves_of_32() {
    // Lane l holds 1.0 (f16) in its low half and l in its high half. The sum
    // starts at lane `hi`'s value, so the result keeps that lane's high half:
    // one in the upper host wave, one in the lower.
    for hi in [40u32, 5] {
        let mut body = vec![
            0x3404_0090, // v_lshlrev_b32 v2, 16, v0
            0x3804_04FF,
            0x0000_3C00, // v_or_b32 v2, 0x3c00, v2
            0xBE96_25C1, // s_or_saveexec_b64 s[22:23], -1
            0xD501_0001,
            0x005A_0480, // v_cndmask_b32 v1, 0, v2, s[22:23]
            0xD760_0004,
            0x101 | (128 + hi) << 9, // v_readlane_b32 s4, v1, hi
            0x7E10_0204,             // v_mov_b32 v8, s4
        ];
        for k in (0..64).filter(|&k| k != hi) {
            body.extend([
                0xD760_0004,
                0x101 | (128 + k) << 9, // v_readlane_b32 s4, v1, k
                0x6410_1004,            // v_add_f16 v8, s4, v8
            ]);
        }
        body.extend([
            0xD760_0005,
            0x0001_0108, // v_readlane_b32 s5, v8, 0
            0xBEFE_0416, // s_mov_b64 exec, s[22:23]
            0x7E10_0205, // v_mov_b32 v8, s5
        ]);
        assert_eq!(waves_required(&body), Some((32, 32)), "two halves, hi {hi}");
        // Lanes 48 and up have their EXEC bit clear: they add nothing.
        for (threads, sum) in [(64, 0x5400), (48, 0x5200)] {
            if let Some(got) = on_waves_of_32(&body, threads) {
                let want = hi << 16 | sum;
                assert_eq!(got, vec![want; threads as usize], "{threads} threads, hi {hi}");
            }
        }
    }
}

#[test]
fn wave64_lane_reads_run_on_two_waves_of_32() {
    let lane = [
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0008,
        0x0002_02C1, // v_mbcnt_hi_u32_b32 v8, -1, v1
    ];
    let mov = 0x7E10_020A; // v_mov_b32 v8, s10
    let reads: [(Vec<u32>, u32); 3] = [
        // v_readlane_b32 s10, v8, 40 (the upper wave's lane 8)
        (vec![0xD760_000A, 0x0001_5108, mov], 40),
        // v_readlane_b32 s10, v8, 33
        (vec![0xD760_000A, 0x0001_4308, mov], 33),
        // v_readfirstlane_b32 s10, v8
        (vec![0x7E14_0508, mov], 0),
    ];
    for threads in [64, 48] {
        for (read, want) in &reads {
            let body = [&lane[..], read].concat();
            if let Some(got) = on_waves_of_32(&body, threads) {
                assert_eq!(got, vec![*want; threads as usize], "{threads} threads, {read:x?}");
            }
        }
    }
}

/// `body` of a wave64 compute program with the skips `skips::find` names
/// left out (as `translate` does where the program still asks for waves of 64).
fn skips_left_out(body: &[u32]) -> Vec<u32> {
    use super::image_tests::{S_ENDPGM, TABLES};
    let head = TABLES.len() + STORE_SETUP.len();
    let words = [&TABLES[..], &STORE_SETUP, body, &STORE, &[S_ENDPGM]].concat();
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let p = crate::ir::build(&code, true, 32).unwrap();
    let skips = super::skips::find(&p);
    let patched = super::skips::leave_out(&code, &skips);
    patched
        .chunks(4)
        .skip(head)
        .take(body.len())
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect()
}

/// The wave sizes a one-wave64 group's program requires, and what each lane stored.
fn waves_required(body: &[u32]) -> Option<(u32, u32)> {
    let words = [&STORE_SETUP[..], body, &STORE[..]].concat();
    let shader = translate_group(&words, (ResourceType::Srv, 8, 0), true, 64, 0).unwrap();
    check(&shader);
    crate::emit::required_waves(&shader.dxil)
}

#[test]
fn a_wave64_skip_over_a_lane_read_is_left_out_for_two_waves_of_32() {
    let lane = [
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0008,
        0x0002_02C1, // v_mbcnt_hi_u32_b32 v8, -1, v1
    ];
    let skip = |body: &[u32]| {
        [
            &lane[..],
            &[
                0x7D88_0088, // v_cmp_gt_u32 vcc, 8, v0
                0xBE96_246A, // s_and_saveexec_b64 s[22:23], vcc
                0xBF88_0000 | body.len() as u32, // s_cbranch_execz over the body
            ],
            body,
            &[0xBEFE_0416], // s_mov_b64 exec, s[22:23]
        ]
        .concat()
    };
    let read = [
        0x7E14_0508, // v_readfirstlane_b32 s10, v8
        0x7E10_020A, // v_mov_b32 v8, s10
    ];
    let body = skip(&read);
    // Lanes deciding alone whether to skip would leave a host wave waiting
    // in the exchange: with the skip kept, both halves decide it together.
    // Lanes 0 to 7 read the first of the lanes that ran: lane 0's.
    let want: Vec<u32> = (0..64).map(|l| if l < 8 { 0 } else { l }).collect();
    assert_eq!(waves_required(&body), Some((32, 32)));
    if let Some(got) = on_waves_of_32(&body, 64) {
        assert_eq!(got, want);
    }
    let left_out = skips_left_out(&body);
    assert_ne!(left_out, body);
    let least = waves_required(&left_out).map(|r| r.0);
    assert!(least.is_none_or(|n| n <= 32), "{least:?}");
    if let Some(got) = on_waves_of_32(&left_out, 64) {
        assert_eq!(got, want);
    }
    // A scalar the body writes and the code after it reads: a skipped body
    // would differ from one that ran, so the skip stays.
    let used = skip(&[
        0x7E14_0508, // v_readfirstlane_b32 s10, v8
        0x7E10_020A, // v_mov_b32 v8, s10
        0xBE8B_0380, // s_mov_b32 s11, 0
    ]);
    let after = [&used[..], &[0x7E12_020B]].concat(); // v_mov_b32 v9, s11
    assert_eq!(skips_left_out(&after), after);
}

/// A host without waves of 32 is asked for waves of 64 where the others get two
/// waves of 32 (`caps::wave64_native`): the same program, either way.
#[test]
fn a_host_without_waves_of_32_is_asked_for_waves_of_64() {
    use super::image_tests::translate_host;
    let body = [
        0xD765_0001,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0xD766_0008,
        0x0002_02C1, // v_mbcnt_hi_u32_b32 v8, -1, v1
    ];
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    let waves = |threads, native| {
        let group = (true, threads, 0);
        let shader = translate_host(&words, (ResourceType::Srv, 8, 0), group, (true, native)).unwrap();
        check(&shader);
        crate::emit::required_waves(&shader.dxil)
    };
    assert_eq!(waves(64, false), Some((32, 32)), "two waves of 32 stand in for one");
    // A group that one wave of 32 holds asks for 32 only where the host has
    // such waves.
    assert_eq!(waves(32, false), Some((32, 32)));
    for threads in [64, 48, 32, 8, 128] {
        assert_eq!(waves(threads, true), Some((64, 64)), "{threads} threads: as the console");
    }
}

#[test]
fn a_mask_set_to_a_constant_then_a_compare_joins() {
    let body = [
        0xBE86_0380, // s_mov_b32 s6, 0
        0x7D88_0090, // v_cmp_gt_u32 vcc_lo, 16, v0
        0xBF06_8100, // s_cmp_eq_u32 s0, 1 (false)
        0xBF85_0001, // s_cbranch_scc1 -> the read
        0xBE86_036A, // s_mov_b32 s6, vcc_lo
        0xD501_0008,
        0x0019_0280, // v_cndmask_b32 v8, 0, 1, s6
    ];
    for (label, got) in stored(&body) {
        let want: Vec<u32> = (0..32).map(|l| u32::from(l < 16)).collect();
        assert_eq!(got, want, "{label}");
    }
}

#[test]
fn a_mask_set_to_a_constant_then_a_compare_joins_in_wave64() {
    // Translation only: the hosts under test run waves of 32.
    let body = [
        0xBE86_0480, // s_mov_b64 s[6:7], 0
        0x7D88_0090, // v_cmp_gt_u32 vcc, 16, v0
        0xBF06_8100, // s_cmp_eq_u32 s0, 1
        0xBF85_0001, // s_cbranch_scc1 -> the read
        0xBE86_046A, // s_mov_b64 s[6:7], vcc
        0xD501_0008,
        0x0019_0280, // v_cndmask_b32 v8, 0, 1, s[6:7]
    ];
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    translate_with(&words, ResourceType::Srv, 4, 0, true).unwrap();
}

/// `body` as one console wave of 64 with `threads` lanes and group-shared
/// memory: it must ask the host for waves of 32 (two halves), and what each
/// thread stored, on hardware with such waves.
fn two_halves_with_lds(body: &[u32], threads: u32) -> Option<Vec<u32>> {
    let words = [&STORE_SETUP[..], body, &STORE[..]].concat();
    let shader = translate_group(&words, (ResourceType::Srv, 8, 0), true, threads, 1).unwrap();
    stored_by_halves(&shader, threads)
}

/// [`two_halves_with_lds`] with the halves on their own conditions only
/// (never following the console's wave together).
fn halves_alone_with_lds(body: &[u32], threads: u32) -> Option<Vec<u32>> {
    let words = [&STORE_SETUP[..], body, &STORE[..]].concat();
    let shader = translate_halves(&words, threads).unwrap();
    stored_by_halves(&shader, threads)
}

/// What each of `threads` threads of `shader` stored, which must ask the
/// host for waves of 32, on hardware with such waves.
fn stored_by_halves(shader: &super::HostShader, threads: u32) -> Option<Vec<u32>> {
    check(shader);
    assert_eq!(
        crate::emit::required_waves(&shader.dxil),
        Some((32, 32)),
        "{threads} threads: {:?}",
        shader.wave64_why
    );
    // WARP runs no fixed wave size of 32.
    if !Host::new(false).is_ok_and(|h| h.has_waves_of(32)) {
        return None;
    }
    let dummy = [[0.0f32; 4]];
    let views = [View::Texture(1, 1, &dummy), View::RawUav(threads.max(64))];
    let mut out = run_on(shader, &views, &[("hardware", false)]);
    Some(out.remove(0).1.remove(0)[..threads as usize].to_vec())
}

/// v17 = 4 × the dword of lane `threads - 1 - lane`.
fn partner(threads: u32) -> [u32; 2] {
    [
        0x4C22_0080 | (threads - 1), // v_sub_nc_u32 v17, threads - 1, v0
        0x3422_2282,                 // v_lshlrev_b32 v17, 2, v17
    ]
}

#[test]
fn a_wave64_branch_only_lower_lanes_hold_takes_both_halves() {
    // Only lanes below 8 hold VCC, so the console's wave does not take
    // `s_cbranch_vccz`: every lane writes and reads, the upper half's too.
    for threads in [64u32, 48] {
        let body = [
            &[0x4A02_0081][..], // v_add_nc_u32 v1, 1, v0
            &partner(threads),
            &[
                0x7E10_0280, // v_mov_b32 v8, 0
                0x7D88_0088, // v_cmp_gt_u32 vcc, 8, v0
                0xBF86_0004, // s_cbranch_vccz past the exchange
                0xD834_0000,
                0x0000_0110, // ds_write_b32 v16, v1
                0xD8D8_0000,
                0x0800_0011, // ds_read_b32 v8, v17
            ],
        ]
        .concat();
        if let Some(got) = two_halves_with_lds(&body, threads) {
            let want: Vec<u32> = (0..threads).map(|l| threads - l).collect();
            assert_eq!(got, want, "{threads} threads");
        }
    }
}

/// The shape of the loops of a compute program that ran for seconds on the
/// host: a scalar count, a skip when it is zero, then a do-while that adds to
/// a vector register, bumps a scalar counter and leaves when the counter
/// equals the count (`s_cmp_eq_u32`, `s_cbranch_scc0`). `count` is set by
/// `set_count` (one instruction), `narrow` wraps it in a lane-mask region.
fn counted_loop(set_count: u32) -> Vec<u32> {
    let body = [
        0xBF06_800C, // s_cmp_eq_u32 s12, 0
        0xBF85_0005, // s_cbranch_scc1 -> after the loop
        0xBE8E_0380, // s_mov_b32 s14, 0
        0x4A10_1081, // v_add_nc_u32 v8, 1, v8
        0x810E_810E, // s_add_i32 s14, s14, 1
        0xBF06_0C0E, // s_cmp_eq_u32 s14, s12
        0xBF84_FFFC, // s_cbranch_scc0 -> the add to v8
    ];
    [&[0x7E10_0280, set_count][..], &body].concat() // v_mov_b32 v8, 0
}

#[test]
fn a_do_while_to_a_scalar_count_runs_that_many_times() {
    for (count, want) in [(6u32, 6u32), (1, 1), (0, 0), (64, 64), (200, 200)] {
        let set = 0xBE8C_0380 | (0x80 + count.min(64)); // s_mov_b32 s12, inline 0..64
        let body = if count <= 64 {
            counted_loop(set)
        } else {
            // A literal.
            let mut b = counted_loop(0xBE8C_03FF);
            b.insert(2, count);
            b
        };
        for (label, got) in stored(&body) {
            assert_eq!(got, [want; 32], "{label} count {count}");
        }
    }
}

/// A loop the console's wave leaves when no lane's counter v2 is below its
/// limit v4 (1 for lanes below 32, 3 above): all lanes run 3 passes, the
/// lower half's too. `pass` is one pass's code; v8 starts at 0.
fn uneven_loop(pass: &[u32]) -> Vec<u32> {
    let mut body = vec![
        0x2C08_0085, // v_lshrrev_b32 v4, 5, v0
        0x3408_0881, // v_lshlrev_b32 v4, 1, v4
        0x4A08_0881, // v_add_nc_u32 v4, 1, v4
        0x7E04_0280, // v_mov_b32 v2, 0
        0x7E10_0280, // v_mov_b32 v8, 0
    ];
    body.extend_from_slice(pass);
    body.extend([
        0x4A04_0481, // v_add_nc_u32 v2, 1, v2
        0x7D88_0504, // v_cmp_gt_u32 vcc, v4, v2
    ]);
    // Back over the pass, the two instructions above and the branch.
    let back = (pass.len() + 3) as u32;
    body.push(0xBF87_0000 | (0x1_0000 - back)); // s_cbranch_vccnz
    body
}

#[test]
fn a_wave64_loop_exchanging_through_lds_keeps_both_halves_in_step() {
    for threads in [64u32, 48] {
        let pass = [
            0xD834_0000,
            0x0000_0110, // ds_write_b32 v16, v1
            0xD8D8_0000,
            0x0300_0011, // ds_read_b32 v3, v17
            0x4A10_1103, // v_add_nc_u32 v8, v3, v8
            0x4A02_02FF,
            100, // v_add_nc_u32 v1, 100, v1
        ];
        let body = [
            &[0x7E02_0300][..], // v_mov_b32 v1, v0
            &partner(threads),
            &uneven_loop(&pass),
        ]
        .concat();
        if let Some(got) = two_halves_with_lds(&body, threads) {
            // Three reads of the partner's lane number plus 0, 100, 200.
            let want: Vec<u32> = (0..threads).map(|l| 3 * (threads - 1 - l) + 300).collect();
            assert_eq!(got, want, "{threads} threads");
        }
    }
}

#[test]
fn a_wave64_ballot_in_a_loop_counts_both_halves_every_pass() {
    for threads in [64u32, 48] {
        let pass = [
            0xD4C4_0018,
            0x0002_00A8, // s[24:25] = lanes below 40
            0xBE8A_1018, // s_bcnt1_i32_b64 s10, s[24:25]
            0x4A10_100A, // v_add_nc_u32 v8, s10, v8
        ];
        if let Some(got) = on_waves_of_32(&uneven_loop(&pass), threads) {
            assert_eq!(got, vec![120; threads as usize], "{threads} threads");
        }
    }
}

#[test]
fn a_wave64_lane_read_under_a_mask_reads_the_other_half() {
    // Lanes below 8 stay in EXEC; `v_readlane` of lane 40 reads it anyway.
    let body = [
        0x7E10_0280, // v_mov_b32 v8, 0
        0x7D88_0088, // v_cmp_gt_u32 vcc, 8, v0
        0xBE96_246A, // s_and_saveexec_b64 s[22:23], vcc
        0xD760_0004,
        0x100 | (128 + 40) << 9, // v_readlane_b32 s4, v0, 40
        0x7E10_0204,             // v_mov_b32 v8, s4
        0xBEFE_0416,             // s_mov_b64 exec, s[22:23]
    ];
    for threads in [64u32, 48] {
        if let Some(got) = on_waves_of_32(&body, threads) {
            let want: Vec<u32> = (0..threads).map(|l| if l < 8 { 40 } else { 0 }).collect();
            assert_eq!(got, want, "{threads} threads");
        }
    }
}

#[test]
fn a_loop_carried_mask_merged_inside_the_loop_is_followed() {
    // The back edge brings a merge of s12 that nothing in the loop reads: it
    // is a mask all the same. The branch inside is never taken past.
    let body = [
        0x7D88_0088, // v_cmp_gt_u32 vcc, 8, v0
        0xBE8C_036A, // s_mov_b32 s12, vcc_lo
        0x7E04_0280, // v_mov_b32 v2, 0
        0x7E10_0280, // v_mov_b32 v8, 0
        0xD501_0001,
        0x0031_0280, // v_cndmask_b32 v1, 0, 1, s12
        0x4A10_1101, // v_add_nc_u32 v8, v1, v8
        0xBE86_0380, // s_mov_b32 s6, 0
        0xBF06_8106, // s_cmp_eq_u32 s6, 1
        0xBF84_0001, // s_cbranch_scc0 over the next
        0x870C_6A0C, // s_and_b32 s12, s12, vcc_lo
        0x4A04_0481, // v_add_nc_u32 v2, 1, v2
        0x7D88_0483, // v_cmp_gt_u32 vcc, 3, v2
        0xBF87_FFF6, // s_cbranch_vccnz to the v_cndmask
    ];
    for (label, got) in stored(&body) {
        let want: Vec<u32> = (0..32).map(|l| if l < 8 { 3 } else { 0 }).collect();
        assert_eq!(got, want, "{label}");
    }
}

#[test]
fn a_do_while_in_a_lane_mask_runs_that_many_times_in_the_lanes_inside() {
    // exec = lanes below 28 around the loop and its skip.
    let inner = counted_loop(0xBE8C_0386); // s_mov_b32 s12, 6
    let mut body = vec![
        0xBE97_037E, // s_mov_b32 s23, exec_lo
        0x7DA8_009C, // v_cmpx_gt_u32 exec_lo, 28, v0
        0xBF88_0000 | inner.len() as u32, // s_cbranch_execz -> the restore
    ];
    body.extend(&inner);
    body.push(0xBEFE_0317); // s_mov_b32 exec_lo, s23
    for (label, got) in stored(&body) {
        let want: Vec<u32> = (0..32).map(|l| if l < 28 { 6 } else { 0 }).collect();
        assert_eq!(got, want, "{label}");
    }
}

#[test]
fn a_do_while_runs_that_many_times_in_wave64() {
    let body = counted_loop(0xBE8C_0386); // s_mov_b32 s12, 6
    // Translation and validation always; execution needs a host with waves of
    // 32 (WARP, and so `VICTORY_WARP=1`, has none).
    let Some(got) = on_waves_of_32(&body, 64) else {
        eprintln!("not executed: no host with waves of 32 (translated and validated only)");
        return;
    };
    assert_eq!(got, [6; 64]);
}

/// The loop a console compiler makes over the lanes of a mask: the ballot of
/// lanes 41 and up, its upper dword walked bit by bit, each bit's number
/// addressing a scalar load. The address comes from lanes, yet it is one for
/// the whole wave, as the ballot is.
#[test]
fn a_wave64_scalar_load_addressed_by_a_ballots_bits_reads_the_same_in_both_halves() {
    let body = [
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x7E10_0280, // v_mov_b32 v8, 0
        0x7D82_00A8, // v_cmp_lt_u32 vcc, 40, v0
        0xBE9A_046A, // s_mov_b64 s[26:27], vcc
        0xBE9E_031B, // s_mov_b32 s30, s27
        0xBE8C_131E, // s_ff1_i32_b32 s12, s30
        0x8F0E_820C, // s_lshl_b32 s14, s12, 2
        0xF420_0344,
        0x1C00_0000, // s_buffer_load_dword s13, s[8:11], s14
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x4A10_100D, // v_add_nc_u32 v8, s13, v8
        0x8F0F_0C81, // s_lshl_b32 s15, 1, s12
        0x891E_0F1E, // s_xor_b32 s30, s30, s15
        0xBF07_801E, // s_cmp_lg_u32 s30, 0
        0xBF85_FFF6, // s_cbranch_scc1 -> the s_ff1
    ];
    let data: Vec<u32> = (0..32).map(|n| 1000 * n).collect();
    for threads in [64u32, 48] {
        let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
        let shader =
            translate_group(&words, (ResourceType::Srv, 4, 0), true, threads, 0).unwrap();
        check(&shader);
        assert_eq!(
            crate::emit::required_waves(&shader.dxil),
            Some((32, 32)),
            "{threads} threads: {:?}",
            shader.wave64_why
        );
        // WARP runs no fixed wave size of 32.
        if !Host::new(false).is_ok_and(|h| h.has_waves_of(32)) {
            return;
        }
        let view = View::Buffer {
            data: &data,
            stride: Stride::Raw,
        };
        let views = [view, View::RawUav(64)];
        let out = run_on(&shader, &views, &[("hardware", false)]).remove(0).1.remove(0);
        // Bits 9 and up of the upper dword, each reading dword `bit`.
        let want = (9..threads - 32).map(|bit| 1000 * bit).sum::<u32>();
        assert_eq!(out[..threads as usize], vec![want; threads as usize], "{threads} threads");
    }
}

/// SCC of a mask operation says whether any lane of the console's wave holds
/// a bit, and a scalar select reads it as such: both halves select alike.
#[test]
fn a_wave64_select_on_the_scc_of_a_mask_operation_sees_both_halves() {
    let body = [
        0x7D82_00A8, // v_cmp_lt_u32 vcc, 40, v0
        0x879C_7E6A, // s_and_b64 s[28:29], vcc, exec
        0x850C_8387, // s_cselect_b32 s12, 7, 3
        0x7E10_020C, // v_mov_b32 v8, s12
        0xD760_0004,
        0x100 | 128 << 9, // v_readlane_b32 s4, v0, 0 (which makes two halves)
        0x4A10_1004,      // v_add_nc_u32 v8, s4, v8: plus 0
    ];
    for threads in [64u32, 48] {
        if let Some(got) = on_waves_of_32(&body, threads) {
            assert_eq!(got, vec![7; threads as usize], "{threads} threads");
        }
    }
}

/// A register the compiler reuses holds a lane mask on one path and data on
/// the other. With both halves following the wave together every edge may be
/// the wave's, so the join takes the mask's number: the ballot of the
/// console's whole wave. Only the wave-flow form is lowered here: on their own
/// conditions lanes do read this join, which that form refuses. Programs get
/// this far where a longer proof shows each lane that it never does.
#[test]
fn a_wave64_join_of_a_mask_and_data_reads_the_ballot_of_both_halves() {
    let body = [
        0x7D82_00A8, // v_cmp_lt_u32 vcc, 40, v0
        0xBE9C_046A, // s_mov_b64 s[28:29], vcc
        0xBF06_8080, // s_cmp_eq_u32 0, 0
        0xBF85_0001, // s_cbranch_scc1 -> the read
        0xBE9D_0385, // s_mov_b32 s29, 5
        0x7E10_021D, // v_mov_b32 v8, s29
    ];
    for threads in [64u32, 48] {
        let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
        let shader = translate_together(&words, threads).unwrap();
        check(&shader);
        assert_eq!(crate::emit::required_waves(&shader.dxil), Some((32, 32)));
        // WARP runs no fixed wave size of 32.
        if !Host::new(false).is_ok_and(|h| h.has_waves_of(32)) {
            return;
        }
        let dummy = [[0.0f32; 4]];
        let views = [View::Texture(1, 1, &dummy), View::RawUav(64)];
        let out = run_on(&shader, &views, &[("hardware", false)]).remove(0).1.remove(0);
        // Lanes 41 and up of those the group has, in the upper dword.
        let want = (41..threads).fold(0, |m, l| m | 1u32 << (l - 32));
        assert_eq!(out[..threads as usize], vec![want; threads as usize], "{threads} threads");
    }
}

/// With both halves, the first lane in EXEC is the console wave's: lane 5,
/// not each host wave's own first (lane 40 in the upper half).
#[test]
fn a_wave64_first_lane_read_is_the_console_waves_first() {
    let body = [
        0x7D84_0085, // v_cmp_eq_u32 vcc, 5, v0
        0xD4C2_001A,
        0x0002_00A8, // v_cmp_eq_u32 s[26:27], 40, v0
        0x88EA_1A6A, // s_or_b64 vcc, vcc, s[26:27]
        0xBE9E_246A, // s_and_saveexec_b64 s[30:31], vcc
        0x7E08_0500, // v_readfirstlane_b32 s4, v0
        0xBEFE_041E, // s_mov_b64 exec, s[30:31]
        0x7E10_0204, // v_mov_b32 v8, s4
        0xD760_0005,
        0x100 | 128 << 9, // v_readlane_b32 s5, v0, 0 (which makes two halves)
        0x4A10_1005,      // v_add_nc_u32 v8, s5, v8: plus 0
    ];
    for threads in [64u32, 48] {
        if let Some(got) = on_waves_of_32(&body, threads) {
            assert_eq!(got, vec![5; threads as usize], "{threads} threads");
        }
    }
}

#[test]
fn a_wave_of_a_wider_group_skips_a_block_another_runs() {
    // Only the first console wave takes the block: it exchanges through LDS
    // within itself and sets s14. The others keep v8 and s14 as they were.
    for threads in [128u32, 96] {
        let body = [
            0x4A02_0081, // v_add_nc_u32 v1, 1, v0
            0x3A22_00BF, // v_xor_b32 v17, 63, v0
            0x3422_2282, // v_lshlrev_b32 v17, 2, v17
            0x7E10_0280, // v_mov_b32 v8, 0
            0xBE8E_0380, // s_mov_b32 s14, 0
            0x7D88_00C0, // v_cmp_gt_u32 vcc, 64, v0
            0xBF86_0006, // s_cbranch_vccz past the block
            0xD834_0000,
            0x0000_0110, // ds_write_b32 v16, v1
            0xD8D8_0000,
            0x0800_0011, // ds_read_b32 v8, v17
            0xD760_000E,
            0x0001_0101, // v_readlane_b32 s14, v1, 0 (1: lane 0)
            0x4A10_100E, // v_add_nc_u32 v8, s14, v8
        ];
        if let Some(got) = two_halves_with_lds(&body, threads) {
            let want: Vec<u32> = (0..threads)
                .map(|l| if l < 64 { (l ^ 63) + 1 + 1 } else { 0 })
                .collect();
            assert_eq!(got, want, "{threads} threads");
        }
    }
}

/// s19 += s18: what the lane read in a loop gives, 64 × the wave.
const LANE_SUM: u32 = 0x8113_1213; // s_add_i32 s19, s19, s18
const ADD_LANE_SUM: u32 = 0x4A10_1013; // v_add_nc_u32 v8, s19, v8

/// v8 = 0, s14 = the console wave's number + 1, s15 = s16 = s19 = s20 = 0.
const TRIPS: [u32; 9] = [
    0x7E10_0280, // v_mov_b32 v8, 0
    0xD760_000E,
    0x0001_0100, // v_readlane_b32 s14, v0, 0 (which makes two halves)
    0x900E_860E, // s_lshr_b32 s14, s14, 6
    0x810E_810E, // s_add_i32 s14, s14, 1
    0xBE8F_0380, // s_mov_b32 s15, 0
    0xBE90_0380, // s_mov_b32 s16, 0
    0xBE94_0380, // s_mov_b32 s20, 0
    0xBE93_0380, // s_mov_b32 s19, 0
];

/// `body` after [`TRIPS`] in groups of 96 and 160 threads (waves of
/// different trip counts, the last with its lower half only): v8 of each lane
/// is `want` of its wave's trip count.
fn waves_of_different_trips(body: &[u32], want: impl Fn(u32) -> u32) {
    waves_of_different_trips_in(body, 1, want);
}

/// [`waves_of_different_trips`] with `blocks` blocks of 128 dwords of LDS.
fn waves_of_different_trips_in(body: &[u32], blocks: u32, want: impl Fn(u32) -> u32) {
    for threads in [96u32, 160] {
        let words = [&STORE_SETUP[..], &TRIPS[..], body, &STORE[..]].concat();
        let shader =
            translate_group(&words, (ResourceType::Srv, 8, 0), true, threads, blocks).unwrap();
        if let Some(got) = stored_by_halves(&shader, threads) {
            let want: Vec<u32> = (0..threads).map(|l| want(l / 64 + 1)).collect();
            assert_eq!(got, want, "{threads} threads");
        }
    }
}

/// A wave that left the loop keeps what it computed in it, scalar and vector,
/// while the group goes round for the others.
#[test]
fn a_wave_of_a_wider_group_keeps_its_values_after_leaving_a_loop() {
    let body = [
        0xD760_0012,
        0x0001_0100, // v_readlane_b32 s18, v0, 0 (an exchange in the loop)
        LANE_SUM,
        0x810F_850F, // s_add_i32 s15, s15, 5
        0x4A10_1081, // v_add_nc_u32 v8, 1, v8
        0x8114_8114, // s_add_i32 s20, s20, 1
        0xBF0A_0E14, // s_cmp_lt_u32 s20, s14
        0xBF85_FFF8, // s_cbranch_scc1 -> the pass
        ADD_LANE_SUM,
        0x4A10_100F, // v_add_nc_u32 v8, s15, v8
    ];
    waves_of_different_trips(&body, |n| n + 5 * n + 64 * (n - 1) * n);
}

#[test]
fn a_wave_of_a_wider_group_breaks_out_of_a_loop() {
    let body = [
        0xD760_0012,
        0x0001_0100, // v_readlane_b32 s18, v0, 0 (an exchange in the loop)
        LANE_SUM,
        0x8114_8114, // s_add_i32 s20, s20, 1
        0x4A10_1081, // v_add_nc_u32 v8, 1, v8
        0xBF06_0E14, // s_cmp_eq_u32 s20, s14
        0xBF85_0003, // s_cbranch_scc1 -> out
        0x810F_850F, // s_add_i32 s15, s15, 5
        0xBF0A_8814, // s_cmp_lt_u32 s20, 8
        0xBF85_FFF6, // s_cbranch_scc1 -> the pass
        ADD_LANE_SUM,
        0x4A10_100F, // v_add_nc_u32 v8, s15, v8
    ];
    waves_of_different_trips(&body, |n| n + 5 * (n - 1) + 64 * (n - 1) * n);
}

/// [`a_wave_of_a_wider_group_breaks_out_of_a_loop`] where the program's LDS
/// takes all of the host's 32 KiB: the group's vote and the lane reads
/// borrow its top, and the 7 each lane keeps in two dwords there (which
/// hold every slot) outlives the loop.
#[test]
fn a_wave_of_a_wider_group_breaks_out_of_a_loop_with_full_shared_memory() {
    let body = [
        0x7E02_0287, // v_mov_b32 v1, 7
        0x3422_0081, // v_lshlrev_b32 v17, 1, v0
        0x4C22_22FF,
        0x0000_1FFE, // v_sub_nc_u32 v17, 8190, v17 (two top dwords a lane)
        0x3422_2282, // v_lshlrev_b32 v17, 2, v17
        0xD834_0000,
        0x0000_0111, // ds_write_b32 v17, v1
        0xD834_0004,
        0x0000_0111, // ds_write_b32 v17, v1 offset:4
        0xD760_0012,
        0x0001_0100, // v_readlane_b32 s18, v0, 0 (an exchange in the loop)
        LANE_SUM,
        0x8114_8114, // s_add_i32 s20, s20, 1
        0x4A10_1081, // v_add_nc_u32 v8, 1, v8
        0xBF06_0E14, // s_cmp_eq_u32 s20, s14
        0xBF85_0003, // s_cbranch_scc1 -> out
        0x810F_850F, // s_add_i32 s15, s15, 5
        0xBF0A_8814, // s_cmp_lt_u32 s20, 8
        0xBF85_FFF6, // s_cbranch_scc1 -> the pass
        ADD_LANE_SUM,
        0x4A10_100F, // v_add_nc_u32 v8, s15, v8
        0xD8D8_0000,
        0x0200_0011, // ds_read_b32 v2, v17
        0xD8D8_0004,
        0x0300_0011, // ds_read_b32 v3, v17 offset:4
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x4A10_1102, // v_add_nc_u32 v8, v2, v8
        0x4A10_1103, // v_add_nc_u32 v8, v3, v8
    ];
    waves_of_different_trips_in(&body, 64, |n| n + 5 * (n - 1) + 64 * (n - 1) * n + 14);
}

/// Three passes of an outer loop, an inner one of each wave's own length in
/// each: what the inner loop computed is read after it and after both.
#[test]
fn a_wave_of_a_wider_group_runs_nested_loops_of_its_own_length() {
    let body = [
        0xBE91_0380, // s_mov_b32 s17, 0
        // The outer pass:
        0xBE94_0380, // s_mov_b32 s20, 0
        // The inner pass:
        0xD760_0012,
        0x0001_0100, // v_readlane_b32 s18, v0, 0 (an exchange in the loop)
        LANE_SUM,
        0x810F_810F, // s_add_i32 s15, s15, 1
        0x4A10_1081, // v_add_nc_u32 v8, 1, v8
        0x8114_8114, // s_add_i32 s20, s20, 1
        0xBF0A_0E14, // s_cmp_lt_u32 s20, s14
        0xBF85_FFF8, // s_cbranch_scc1 -> the inner pass
        0x8110_0F10, // s_add_i32 s16, s16, s15
        0x8111_8111, // s_add_i32 s17, s17, 1
        0xBF0A_8311, // s_cmp_lt_u32 s17, 3
        0xBF85_FFF3, // s_cbranch_scc1 -> the outer pass
        ADD_LANE_SUM,
        0x4A10_100F, // v_add_nc_u32 v8, s15, v8
        0x4A10_1010, // v_add_nc_u32 v8, s16, v8
    ];
    // v8 and s15 count 3n; s16 sums s15 after each outer pass.
    waves_of_different_trips(&body, |n| 3 * n + 3 * n + 6 * n + 64 * (n - 1) * 3 * n);
}

/// A wave that is off in a block writes no shared memory there.
#[test]
fn a_wave_of_a_wider_group_writes_no_shared_memory_where_it_is_off() {
    // Every lane clears its dword; only the first console wave takes the
    // block that writes it (and reads a lane, which makes two halves).
    for threads in [128u32, 96] {
        let body = [
            0x4A02_0081, // v_add_nc_u32 v1, 1, v0
            0x7E04_0280, // v_mov_b32 v2, 0
            0xBE8E_0380, // s_mov_b32 s14, 0
            0xD834_0000,
            0x0000_0210, // ds_write_b32 v16, v2
            0x7D88_00C0, // v_cmp_gt_u32 vcc, 64, v0
            0xBF86_0004, // s_cbranch_vccz past the write and the lane read
            0xD834_0000,
            0x0000_0110, // ds_write_b32 v16, v1
            0xD760_000E,
            0x0001_0101, // v_readlane_b32 s14, v1, 0 (1: lane 0)
            0xD8D8_0000,
            0x0800_0010, // ds_read_b32 v8, v16
            0x4A10_100E, // v_add_nc_u32 v8, s14, v8
        ];
        if let Some(got) = two_halves_with_lds(&body, threads) {
            let want: Vec<u32> = (0..threads)
                .map(|l| if l < 64 { l + 2 } else { 0 })
                .collect();
            assert_eq!(got, want, "{threads} threads");
        }
    }
}

/// The two halves' slots take 132 dwords for 128 threads with a lane read:
/// past the program's own with 62 blocks of 128, in its top where it takes
/// all 64 (the host's 32 KiB). Each lane writes its id + 1 to two dwords of
/// the program's top 256, which hold every slot; reads its wave's lane mask
/// of `id < 64` as a number (the halves exchange their parts) and lane 0's
/// id (a lane read across them); then reads both dwords back: the program's
/// values outlive both either way. With 65 blocks the program's own is over
/// the host's 32 KiB: it is not run as two halves.
#[test]
fn a_wave64_program_keeps_its_shared_memory_across_exchanges() {
    let threads = 128;
    for blocks in [62, 64, 65] {
        let body = [
            0x4A02_0081, // v_add_nc_u32 v1, 1, v0
            0x3422_0081, // v_lshlrev_b32 v17, 1, v0
            0x4C22_22FF,
            blocks * 128 - 2, // v_sub_nc_u32 v17, top - 1, v17 (two dwords a lane)
            0x3422_2282,      // v_lshlrev_b32 v17, 2, v17
            0xD834_0000,
            0x0000_0111, // ds_write_b32 v17, v1
            0xD834_0004,
            0x0000_0111, // ds_write_b32 v17, v1 offset:4
            0x7D88_00C0, // v_cmp_gt_u32 vcc, 64, v0
            0xBE8F_106A, // s_bcnt1_i32_b64 s15, vcc (the mask as a number)
            0xD760_000E,
            0x0001_0100, // v_readlane_b32 s14, v0, 0 (the console wave's first id)
            0xD8D8_0000,
            0x0800_0011, // ds_read_b32 v8, v17
            0xD8D8_0004,
            0x0200_0011, // ds_read_b32 v2, v17 offset:4
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0x4A10_1102, // v_add_nc_u32 v8, v2, v8
            0x4A10_100E, // v_add_nc_u32 v8, s14, v8
            0x4A10_100F, // v_add_nc_u32 v8, s15, v8
        ];
        let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
        let shader =
            translate_group(&words, (ResourceType::Srv, 8, 0), true, threads, blocks).unwrap();
        if blocks > 64 {
            // (The validator refuses that size whatever the halves do.)
            assert_eq!(
                crate::emit::required_waves(&shader.dxil),
                Some((64, 64)),
                "{:?}",
                shader.wave64_why
            );
            assert!(shader.wave64_why.is_some());
            continue;
        }
        if let Some(got) = stored_by_halves(&shader, threads) {
            let want: Vec<u32> = (0..threads)
                .map(|l| 2 * (l + 1) + l / 64 * 64 + if l < 64 { 64 } else { 0 })
                .collect();
            assert_eq!(got, want, "{blocks} blocks");
        }
    }
}

/// The prefix of wave totals the console's structure builder computes: each
/// console wave of a 256-thread group writes its total (10 + its index) to
/// LDS, then, under `if (id >> 6 != 0)`, loops over the totals of the waves
/// before it with lanes leaving at their own wave's index, reading the
/// running sum's first lane (`v_readfirstlane`) every iteration. The total is
/// read from the wave's last lane first, a read across the two host waves.
/// `region` goes first under the `if`. Every thread stores the sum of the
/// waves before its own: 0, 10, 10 + 11, 10 + 11 + 12.
/// Adapted from avatarmanz/VIctory-7900XTX 431cab5.
fn wave_prefix(region: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let skip = 0x11 + region.len() as u32;
    let body = [
        &[
            0x2C0E_0086, // v_lshrrev_b32 v7, 6, v0     (the console wave's index)
            0x3410_0E82, // v_lshlrev_b32 v8, 2, v7
            0x4A12_0E8A, // v_add_nc_u32 v9, 10, v7
            0xD760_0013,
            0x0201_7F09, // v_readlane_b32 s19, v9, 63 (the wave's last lane)
            0x7E12_0213, // v_mov_b32 v9, s19
            0xD834_0000,
            0x0000_0908,        // ds_write_b32 v8, v9
            0xBF8C_C07F,        // s_waitcnt lgkmcnt(0)
            0xBF8A_0000,        // s_barrier
            0x7E10_0280,        // v_mov_b32 v8, 0
            0xBE80_047E,        // s_mov_b64 s[0:1], exec
            0x7DAA_0E80,        // v_cmpx_ne_u32 0, v7
            0xBF88_0000 | skip, // s_cbranch_execz END
        ][..],
        region,
        &[
            0xBE86_047E, // s_mov_b64 s[6:7], exec
            0xBE92_0380, // s_mov_b32 s18, 0
            0xBE91_0380, // s_mov_b32 s17, 0
            0xBF82_0002, // s_branch BODY
            0x87FE_0614, // LOOPTOP: s_and_b64 exec, s[20:21], s[6:7]
            0x7E22_0508, // v_readfirstlane_b32 s17, v8
            0x8F10_8212, // BODY: s_lshl_b32 s16, s18, 2
            0x8112_8112, // s_add_i32 s18, s18, 1
            0x7E10_0210, // v_mov_b32 v8, s16
            0xD8D8_0000,
            0x0800_0008, // ds_read_b32 v8, v8
            0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
            0x4A10_1011, // v_add_nc_u32 v8, s17, v8
            0xBE94_047E, // s_mov_b64 s[20:21], exec
            0x7DA4_0E12, // v_cmpx_eq_u32 s18, v7
            0x8A86_7E06, // s_andn2_b64 s[6:7], s[6:7], exec
            0xBF85_FFF3, // s_cbranch_scc1 LOOPTOP
            0xBEFE_0400, // END: s_mov_b64 exec, s[0:1]
        ],
    ]
    .concat();
    let want = (0..256u32)
        .map(|t| (0..t / 64).map(|w| 10 + w).sum())
        .collect();
    (body, want)
}

/// The condition and the sum are alike in every lane of a console wave, so
/// each host wave takes its own first lane, where an exchange (in a loop)
/// would be refused. With both halves following the console's wave.
#[test]
fn a_first_lane_read_of_a_wave_alike_sum_under_a_wave_alike_loop() {
    let (body, want) = wave_prefix(&[]);
    if let Some(got) = two_halves_with_lds(&body, 256) {
        assert_eq!(got, want);
    }
}

/// [`a_first_lane_read_of_a_wave_alike_sum_under_a_wave_alike_loop`] with
/// the halves on their own conditions (not following the console's wave), as
/// in the console's structure builder when it took that form: each lane on
/// its own conditions.
#[test]
fn a_first_lane_read_of_a_wave_alike_sum_in_a_loop_of_lanes_on_their_own() {
    let (body, want) = wave_prefix(&[]);
    if let Some(got) = halves_alone_with_lds(&body, 256) {
        assert_eq!(got, want);
    }
}

/// [`a_first_lane_read_of_a_wave_alike_sum_in_a_loop_of_lanes_on_their_own`]
/// (the halves on their own) with the sum kept in v11 (no first-lane read in
/// the loop) and odd lanes
/// going round once more (to their own wave's total), then a first-lane read
/// of the sum after the loop: the halves exchange there, and the console's
/// first lane (an even one) gives every lane the sum of the waves before its
/// own. (The sum needs itself, so it is never proven alike.)
#[test]
fn a_first_lane_read_after_a_loop_lanes_leave_apart_takes_the_first_lane() {
    let (mut body, want) = wave_prefix(&[
        0x3614_0081, // v_and_b32 v10, 1, v0
        0x4A14_0F0A, // v_add_nc_u32 v10, v10, v7
    ]);
    for (old, new) in [
        (0x7E10_0280, 0x7E16_0280), // v_mov_b32 v11, 0 (for v8)
        (0x7E22_0508, 0xBF80_0000), // s_nop 0 (for the first-lane read)
        (0x4A10_1011, 0x4A16_110B), // v_add_nc_u32 v11, v11, v8
        (0x7DA4_0E12, 0x7DA4_1412), // v_cmpx_eq_u32 s18, v10
    ] {
        let at = body.iter().position(|&w| w == old).unwrap();
        body[at] = new;
    }
    body.extend([
        0x7E3C_050B, // v_readfirstlane_b32 s30, v11
        0x7E10_021E, // v_mov_b32 v8, s30
    ]);
    if let Some(got) = halves_alone_with_lds(&body, 256) {
        assert_eq!(got, want);
    }
}

/// The loop of [`a_first_lane_read_after_a_loop_lanes_leave_apart_takes_the_first_lane`]
/// alone, the halves on their own: its first-lane read is not taken per host
/// wave, since each half would carry its own last result out of a loop its
/// lanes leave apart; the exchange it needs cannot be in a loop there, so
/// that form asks for waves of 64.
#[test]
fn a_first_lane_read_in_a_loop_lanes_leave_apart_is_not_taken_per_half() {
    let (mut body, _) = wave_prefix(&[
        0x3614_0081, // v_and_b32 v10, 1, v0
        0x4A14_0F0A, // v_add_nc_u32 v10, v10, v7
    ]);
    let exit = body.iter().position(|&w| w == 0x7DA4_0E12).unwrap();
    body[exit] = 0x7DA4_1412; // v_cmpx_eq_u32 s18, v10
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    let shader = translate_halves(&words, 256).unwrap();
    assert_eq!(
        crate::emit::required_waves(&shader.dxil),
        Some((64, 64)),
        "{:?}",
        shader.wave64_why
    );
}

/// The loop of [`a_first_lane_read_in_a_loop_lanes_leave_apart_is_not_taken_per_half`]
/// with a scalar load in the block the first console wave skips: the halves
/// on their own refuse it, so the group runs every block, the first wave off
/// in that one (module `linear`), its load not made. Each lane's first-lane
/// read is the console's; even lanes leave with the sum of the waves before
/// their own, odd lanes, a round later, with their own wave's total added;
/// the first wave stores 0.
// shortcut: the load's result is unused, so nothing here shows that a wave
// that is on gets it after the guarded code.
#[test]
fn a_first_lane_read_in_a_loop_lanes_leave_apart_and_a_load_take_both_halves_together() {
    let (mut body, _) = wave_prefix(&[
        0xF408_0B02,
        0xFA00_0020, // s_load_dwordx4 s[44:47], s[4:5], 0x20 (unused)
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x3614_0081, // v_and_b32 v10, 1, v0
        0x4A14_0F0A, // v_add_nc_u32 v10, v10, v7
    ]);
    let exit = body.iter().position(|&w| w == 0x7DA4_0E12).unwrap();
    body[exit] = 0x7DA4_1412; // v_cmpx_eq_u32 s18, v10
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    assert_eq!(
        crate::emit::required_waves(&translate_halves(&words, 256).unwrap().dxil),
        Some((64, 64)),
        "the halves on their own"
    );
    let want: Vec<u32> = (0..256u32)
        .map(|t| match t / 64 {
            0 => 0,
            w => (0..w + t % 2).map(|j| 10 + j).sum(),
        })
        .collect();
    if let Some(got) = two_halves_with_lds(&body, 256) {
        assert_eq!(got, want);
    }
}

/// A value a loop's head writes from its counter, with the upper 32 lanes
/// going round once more, then a first-lane read of it after the loop: each
/// lane holds its own last iteration's value, and the console's first lane
/// (lane 0, which left first) gives every lane 0, the upper host wave too.
/// (A lane read first makes the console wave two halves of one wave.)
#[test]
fn a_loop_counter_copy_read_after_lanes_leave_apart_takes_the_first_lane() {
    let body = [
        0xD760_0013,
        0x0201_7F00, // v_readlane_b32 s19, v0, 63 (unused)
        0x2C06_0085, // v_lshrrev_b32 v3, 5, v0
        0x4A06_0681, // v_add_nc_u32 v3, 1, v3
        0xBE82_0380, // s_mov_b32 s2, 0
        0xBE80_047E, // s_mov_b64 s[0:1], exec
        0x7E02_0202, // L: v_mov_b32 v1, s2
        0x8002_8102, // s_add_u32 s2, s2, 1
        0x7DA2_0602, // v_cmpx_lt_u32 s2, v3
        0xBF89_FFFC, // s_cbranch_execnz L
        0xBEFE_0400, // s_mov_b64 exec, s[0:1]
        0x7E3C_0501, // v_readfirstlane_b32 s30, v1
        0x7E10_021E, // v_mov_b32 v8, s30
    ];
    let words = [&STORE_SETUP[..], &body, &STORE[..]].concat();
    let shader = translate_group(&words, (ResourceType::Srv, 8, 0), true, 64, 0).unwrap();
    assert_eq!(crate::emit::required_waves(&shader.dxil), Some((32, 32)));
    if let Some(got) = on_waves_of_32(&body, 64) {
        assert_eq!(got, vec![0; 64]);
    }
}
