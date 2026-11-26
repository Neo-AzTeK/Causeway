//! The fake-caps matrix: every planner against every wave range, tier and
//! failure, with an oracle written from the rule, not from the planner.

use super::*;
use crate::caps::reference;

const WAVES: &[Option<(u32, u32)>] = &[
    None,
    Some((32, 32)),
    Some((32, 64)),
    Some((64, 64)),
    Some((8, 32)),
    Some((16, 16)),
    Some((4, 4)),
    Some((4, 128)),
];
const SMS: &[Option<u32>] = &[None, Some(0x60), Some(0x65), Some(0x66), Some(0x67)];
const TIERS: &[Option<u8>] = &[None, Some(0), Some(1), Some(2), Some(3), Some(4)];
const NEEDS: &[(u32, u32)] = &[(32, 32), (64, 64), (32, 64), (16, 16), (8, 128)];

fn with_wave(w: Option<(u32, u32)>, sm: Option<u32>) -> Caps {
    Caps {
        wave: w.map(|(min, max)| WaveLanes { min, max }),
        shader_model: sm.map(ShaderModel),
        ..Caps::unknown()
    }
}

#[test]
fn wave_plan_matches_the_rule_everywhere() {
    for &w in WAVES {
        for &sm in SMS {
            for &(min, max) in NEEDS {
                for pinnable in [true, false] {
                    let caps = with_wave(w, sm);
                    let plan = wave_plan(&caps, WaveNeed::Lanes { min, max, pinnable });
                    let case = format!("host {w:?} sm {sm:x?} need {min}..{max} pin {pinnable}: {plan:?}");
                    let pin_ok = pinnable && sm.is_some_and(|m| m >= 0x66);
                    // The rule, written out: sizes the host can run that the
                    // shader accepts; any host choice acceptable -> nothing
                    // to ask; else only a pin works.
                    let sizes: Vec<u32> = [4, 8, 16, 32, 64, 128]
                        .into_iter()
                        .filter(|n| (min..=max).contains(n))
                        .filter(|n| w.is_some_and(|(a, b)| (a..=b).contains(n)))
                        .collect();
                    match w {
                        None => assert!(matches!(plan, WavePlan::Refuse(_)), "{case}"),
                        Some((a, b)) if min <= a && b <= max => assert_eq!(plan, WavePlan::Native, "{case}"),
                        Some(_) if sizes.is_empty() || !pin_ok => {
                            assert!(matches!(plan, WavePlan::Refuse(_)), "{case}")
                        }
                        Some(_) => match plan {
                            WavePlan::Pinned(n) => assert!(sizes.contains(&n), "{case}"),
                            _ => panic!("{case}"),
                        },
                    }
                    if let WavePlan::Refuse(r) = plan {
                        assert!(r.to_string().starts_with("the host GPU lacks "), "{case}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_host_range_inside_the_need_needs_no_pin() {
    let mut rdna = reference::amd_rdna();
    rdna.shader_model = Some(ShaderModel(0x65));
    let any = WaveNeed::Lanes { min: 32, max: 64, pinnable: false };
    assert_eq!(wave_plan(&rdna, any), WavePlan::Native);
    // Wave64 in a stage that cannot pin, on a range host: refuse.
    let w64 = WaveNeed::Lanes { min: 64, max: 64, pinnable: false };
    assert!(matches!(wave_plan(&reference::amd_rdna(), w64), WavePlan::Refuse(_)));
}

#[test]
fn unknown_uma_does_not_shrink_the_budget_to_a_window() {
    let mut c = reference::intel_igpu();
    c.memory.budget = None;
    c.uma = None;
    assert_eq!(memory_plan(&c).budget, 8 << 30);
}

#[test]
fn wave64_work_is_never_native_on_a_wave32_host() {
    for caps in [reference::nvidia_modern(), reference::nvidia_pascal(), reference::warp()] {
        let plan = wave_plan(&caps, WaveNeed::Lanes { min: 64, max: 64, pinnable: true });
        assert!(matches!(plan, WavePlan::Refuse(_)), "{}", caps.adapter.name);
    }
    let need32 = WaveNeed::Lanes { min: 32, max: 32, pinnable: true };
    assert!(matches!(wave_plan(&reference::amd_gcn(), need32), WavePlan::Refuse(_)));
}

#[test]
fn wave_plan_serves_each_reference_class() {
    let n64 = WaveNeed::Lanes { min: 64, max: 64, pinnable: true };
    let n32 = WaveNeed::Lanes { min: 32, max: 32, pinnable: true };
    assert_eq!(wave_plan(&reference::amd_gcn(), n64), WavePlan::Native);
    assert_eq!(wave_plan(&reference::amd_rdna(), n64), WavePlan::Pinned(64));
    assert_eq!(wave_plan(&reference::amd_rdna(), n32), WavePlan::Pinned(32));
    assert_eq!(wave_plan(&reference::intel_arc(), n32), WavePlan::Pinned(32));
    assert_eq!(wave_plan(&reference::nvidia_modern(), n32), WavePlan::Native);
    // A range host without shader model 6.6 cannot pin.
    let mut old = reference::amd_rdna();
    old.shader_model = Some(ShaderModel(0x65));
    assert!(matches!(wave_plan(&old, n64), WavePlan::Refuse(_)));
    assert_eq!(wave_plan(&Caps::unknown(), WaveNeed::Any), WavePlan::Native);
}

#[test]
fn unknown_means_unsupported() {
    let c = Caps::unknown();
    assert!(!min_lod_clamp(&c) && !mesh_shaders(&c) && !atomics64_heap(&c) && !aniso_point_mip(&c));
    assert!(matches!(wave_plan(&c, WaveNeed::Lanes { min: 32, max: 32, pinnable: true }), WavePlan::Refuse(_)));
    assert_eq!(binding_plan(&c).class, BindingClass::Slots);
    assert_eq!(memory_plan(&c).source, BudgetSource::Fallback);
    assert!(format_choice(&c, &[(28, 1)]).is_err());
}

#[test]
fn tiers_are_compared_with_at_least() {
    for &t in TIERS {
        let c = Caps { tiled_tier: t, ..Caps::unknown() };
        assert_eq!(min_lod_clamp(&c), t.is_some_and(|t| t >= 2), "tiled {t:?}");
        let c = Caps { binding_tier: t, ..Caps::unknown() };
        let want = match t {
            Some(t) if t >= 3 => BindingClass::Bindless,
            Some(2) => BindingClass::Tables,
            _ => BindingClass::Slots,
        };
        assert_eq!(binding_plan(&c).class, want, "binding {t:?}");
        for &sm in SMS {
            let c = Caps { mesh_tier: t, shader_model: sm.map(ShaderModel), ..Caps::unknown() };
            assert_eq!(mesh_shaders(&c), t.is_some_and(|t| t >= 1) && sm.is_some_and(|m| m >= 0x66));
        }
    }
}

#[test]
fn a_better_device_never_gets_a_worse_plan() {
    // Binding: limits only grow with the tier.
    let rank = |t| match binding_plan(&Caps { binding_tier: t, ..Caps::unknown() }).class {
        BindingClass::Slots => 0,
        BindingClass::Tables => 1,
        BindingClass::Bindless => 2,
    };
    for t in 0u8..5 {
        assert!(rank(Some(t + 1)) >= rank(Some(t)), "tier {t}");
    }
    assert!(rank(Some(1)) >= rank(None));
    // Wave: widening the host range or raising the model never turns a
    // plan into a refusal.
    for &(min, max) in NEEDS {
        let need = WaveNeed::Lanes { min, max, pinnable: true };
        for &sm in SMS {
            let narrow = wave_plan(&with_wave(Some((32, 32)), sm), need);
            let wide = wave_plan(&with_wave(Some((8, 128)), sm), need);
            if !matches!(narrow, WavePlan::Refuse(_)) && sm.is_some_and(|m| m >= 0x66) {
                assert!(!matches!(wide, WavePlan::Refuse(_)), "need {min}..{max} sm {sm:?}");
            }
        }
    }
}

#[test]
fn plans_do_not_depend_on_vendor_or_driver() {
    for base in reference::all() {
        let mut other = base.clone();
        other.adapter = AdapterInfo {
            name: "other".into(),
            vendor_id: 0xdead,
            device_id: 0xbeef,
            driver_version: Some(1),
            software: !base.adapter.software,
        };
        for &(min, max) in NEEDS {
            let need = WaveNeed::Lanes { min, max, pinnable: true };
            assert_eq!(wave_plan(&base, need), wave_plan(&other, need), "{}", base.adapter.name);
        }
        assert_eq!(binding_plan(&base), binding_plan(&other));
        assert_eq!(memory_plan(&base), memory_plan(&other));
        assert_eq!(min_lod_clamp(&base), min_lod_clamp(&other));
        assert_eq!(mesh_shaders(&base), mesh_shaders(&other));
        assert_eq!(atomics64_heap(&base), atomics64_heap(&other));
        assert_eq!(aniso_point_mip(&base), aniso_point_mip(&other));
    }
}

#[test]
fn heap_atomics_need_both_caps() {
    const FLAGS: [Option<bool>; 3] = [None, Some(false), Some(true)];
    for typed_atomics64 in FLAGS {
        for heap_atomics64 in FLAGS {
            let c = Caps { typed_atomics64, heap_atomics64, ..Caps::unknown() };
            let both = typed_atomics64 == Some(true) && heap_atomics64 == Some(true);
            assert_eq!(atomics64_heap(&c), both, "{typed_atomics64:?} {heap_atomics64:?}");
        }
    }
    assert!(!atomics64_heap(&reference::intel_arc()) && atomics64_heap(&reference::amd_rdna()));
}

#[test]
fn point_mip_aniso_is_used_only_where_the_device_says_so() {
    for (said, want) in [(None, false), (Some(false), false), (Some(true), true)] {
        let c = Caps { aniso_point_mip: said, ..Caps::unknown() };
        assert_eq!(aniso_point_mip(&c), want, "{said:?}");
    }
}

#[test]
fn the_digest_says_what_the_caps_hold() {
    assert_eq!(
        reference::amd_rdna().digest(),
        "wave 32-64, binding tier 3, mesh tier 1, shader model 0x67"
    );
    assert_eq!(Caps::unknown().digest(), "wave n/a, binding tier n/a, mesh tier n/a, shader model n/a");
}

#[test]
fn memory_budget_is_chosen_from_the_figures_it_has() {
    let gib = 1u64 << 30;
    // Query worked: that wins, on every class.
    for c in reference::all() {
        assert_eq!(memory_plan(&c).source, BudgetSource::Queried, "{}", c.adapter.name);
    }
    // Query failed: a discrete card uses its own memory, a UMA part half
    // its shared memory (not its tiny dedicated figure).
    let mut d = reference::nvidia_modern();
    d.memory.budget = None;
    assert_eq!(memory_plan(&d), MemoryPlan { budget: 8 * gib, source: BudgetSource::Dedicated });
    let mut u = reference::intel_igpu();
    u.memory.budget = None;
    assert_eq!(memory_plan(&u), MemoryPlan { budget: 8 * gib, source: BudgetSource::SharedHalf });
    // A zero budget is a failed query. Nothing reported: the fallback.
    u.memory.budget = Some(0);
    u.memory.shared = 0;
    assert_eq!(memory_plan(&u).budget, planner::FALLBACK_BUDGET);
    // A small card gets a small budget, not a constant.
    let mut s = reference::nvidia_pascal();
    s.memory.budget = Some(2 * gib);
    assert_eq!(memory_plan(&s).budget, 2 * gib);
}

#[test]
fn formats_fall_back_in_order_and_unprobed_is_unsupported() {
    let mut c = reference::amd_rdna();
    c.formats.insert(98, 0b111); // preferred, all bits
    c.formats.insert(28, 0b011); // fallback, fewer bits
    assert_eq!(format_choice(&c, &[(98, 0b100), (28, 0b001)]), Ok(98));
    assert_eq!(format_choice(&c, &[(98, 0b1000), (28, 0b001)]), Ok(28));
    assert!(format_choice(&c, &[(99, 1)]).is_err());
    assert!(!c.format_supports(99, 0));
}

#[test]
fn workarounds_are_bounded_and_correct_the_caps() {
    for w in WORKAROUNDS {
        assert!(!w.bug.is_empty() && w.driver_from < w.driver_below, "{}", w.id);
    }
    fn no_mesh(c: &mut Caps) {
        c.mesh_tier = Some(0);
    }
    let table = [Workaround {
        id: "test-no-mesh",
        vendor_id: 0x1002,
        device_id: None,
        driver_from: 10,
        driver_below: 20,
        bug: "test",
        fix: no_mesh,
    }];
    let mut hit = reference::amd_rdna();
    hit.adapter.driver_version = Some(15);
    assert_eq!(hit.apply_workarounds(&table), 1);
    assert!(!mesh_shaders(&hit) && hit.workarounds == ["test-no-mesh"]);
    for (vendor, driver) in [(0x10de, Some(15)), (0x1002, Some(20)), (0x1002, Some(9)), (0x1002, None)] {
        let mut miss = reference::amd_rdna();
        miss.adapter.vendor_id = vendor;
        miss.adapter.driver_version = driver;
        assert_eq!(miss.apply_workarounds(&table), 0, "{vendor:#x} {driver:?}");
        assert!(mesh_shaders(&miss));
    }
}

#[test]
fn generations_increase_and_refusals_read_cleanly() {
    let (a, b) = (Caps::next_generation(), Caps::next_generation());
    assert!(b > a);
    let r = Refusal::new("wave size", "waves of 64 to 64 lanes", "32 to 32");
    assert_eq!(r.to_string(), "the host GPU lacks wave size: needs waves of 64 to 64 lanes, has 32 to 32");
    assert!(reference::warp().summary().contains("software"));
}

#[test]
fn doubles_and_barycentrics_need_the_device_to_say_so() {
    for (said, want) in [(Some(true), true), (Some(false), false), (None, false)] {
        let c = Caps { doubles: said, ..Caps::unknown() };
        assert_eq!(doubles(&c), want, "{said:?}");
        let c = Caps { barycentrics: said, ..Caps::unknown() };
        assert_eq!(barycentrics(&c), want, "{said:?}");
    }
}
