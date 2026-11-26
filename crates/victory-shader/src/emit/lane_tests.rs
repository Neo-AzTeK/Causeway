//! End-to-end tests of lane masks and scalar values as each lane sees them:
//! synthetic GFX10 programs of 32 lanes, run as in `image_tests`.

use super::host::{Host, Stride, View};
use super::image_tests::{build, check, run, run_on, translate, translate_group, translate_with};
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
    let views = [View::Texture(1, 1, &dummy), View::RawUav(64)];
    let mut out = run_on(&shader, &views, &[("hardware", false)]);
    Some(out.remove(0).1.remove(0)[..threads as usize].to_vec())
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
    // Lanes 48 and up have their EXEC bit clear.
    for threads in [64, 48] {
        let n = threads as usize;
        let want: [(&[u32], Vec<u32>); 3] = [
            (&lane, (0..n as u32).collect()),
            (&[&ballot[..], &count].concat(), vec![40; n]),
            (&[&ballot[..], &below].concat(), (0..n as u32).map(|l| l.min(40)).collect()),
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
    // Lanes decide alone whether to skip: a host wave may leave its partner
    // waiting in the exchange.
    assert_eq!(waves_required(&body), Some((64, 64)));
    let left_out = skips_left_out(&body);
    assert_ne!(left_out, body);
    let least = waves_required(&left_out).map(|r| r.0);
    assert!(least.is_none_or(|n| n <= 32), "{least:?}");
    if let Some(got) = on_waves_of_32(&left_out, 64) {
        // Lanes 0 to 7 read the first of the lanes that ran: lane 0's.
        let want: Vec<u32> = (0..64).map(|l| if l < 8 { 0 } else { l }).collect();
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
    let waves = |native| {
        let shader = translate_host(&words, (ResourceType::Srv, 8, 0), (true, 64, 0), (true, native)).unwrap();
        check(&shader);
        crate::emit::required_waves(&shader.dxil)
    };
    assert_eq!(waves(false), Some((32, 32)), "two waves of 32 stand in for one");
    assert_eq!(waves(true), Some((64, 64)), "waves of 64 as the console runs them");
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
