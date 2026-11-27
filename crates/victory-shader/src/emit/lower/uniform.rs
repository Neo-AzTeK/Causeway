//! VGPR values alike in every lane of a console wave, for programs of waves
//! of 64 run as two host waves of 32.
//!
//! The compiler keeps wave-uniform data in VGPRs and fetches it with
//! `v_readfirstlane`. Where the value is the same in every lane of the
//! console wave, each host wave's own lane gives the console's first lane, so
//! the two halves need not meet (an exchange, which needs every thread of the
//! group to arrive once: none in a loop or a block a branch skips).
//!
//! A value is alike when it is written lane by lane from alike inputs: VGPRs
//! alike themselves, scalars no lane gave ([`super::super::halves`]'s
//! lane-derived ones are not), constants. An instruction under a lane guard
//! counts only if the guard is alike (a console wave runs it in all its lanes
//! or none) and the old value is alike too, as IR reads it for every VGPR
//! write. An LDS read at an alike address reads one dword in every lane. The
//! thread id's x shifted right by 6 or more is alike where a console wave is
//! one run of 64 ids along x ([`wave_tid`]).
//!
//! A phi counts where every predecessor is a block that the lanes of a console
//! wave reach together: no branch on a condition that is not alike lies
//! between it and the entry. Loop-carried values count when what the loop
//! carries is alike; the analysis grows from nothing to a fixed point, so a
//! value that needs itself to be alike never is.
//!
//! Adapted from avatarmanz/VIctory-7900XTX 431cab5, 7348b80 and abbbd6b.
// shortcut: two halves may read LDS at different times; where a console wave
// writes and reads the same dword with no barrier between, the halves could
// read different values. Every lane's own read has that hazard already.

use std::collections::HashSet;

use crate::emit::halves;
use crate::gfx10::{Fields, NULL, Operand};
use crate::io::{Input, Io};
use crate::ir::{Program, Reg, Value};
use crate::native::{NativeProgram, StageIo};
use crate::structure::{Atom, Bool, LaneExit, Structured};

/// The thread id's x of a compute program of waves of 64 whose console waves
/// are runs of 64 ids along x (a group 64 wide along x, or one row): there
/// `x >> 6` and up is one value per console wave, as the host waves of a
/// console wave hold its 64 flat ids (`Lower::halves_prologue`).
pub(super) fn wave_tid(native: &NativeProgram, io: &Io) -> Option<Value> {
    let StageIo::Cs(cs) = &native.io else {
        return None;
    };
    let [tx, ty, tz] = cs.threads;
    if !native.wave64 || !(tx % 64 == 0 || ty * tz == 1) {
        return None;
    }
    io.inputs
        .iter()
        .find(|(_, i)| matches!(i, Input::ThreadIdInGroup(0)))
        .map(|&(v, _)| v)
}

/// The values (VGPRs, and the compare results their conditions are made of)
/// that are the same in every lane of a console wave holding them.
pub(super) fn lane_uniform(
    p: &Program,
    st: &Structured,
    wave_tid: Option<Value>,
) -> HashSet<Value> {
    let derived = halves::lane_derived(p);
    let regions = lane_regions(st);
    let mut same: HashSet<Value> = HashSet::new();
    loop {
        let alike_bool = |same: &HashSet<Value>, b: Bool| {
            st.bools.support(b).iter().all(|a| match *a {
                // An SCC no lane gave is one value in both halves.
                Atom::Uniform(v) => !derived.contains(&v),
                Atom::Lane(v) => same.contains(&v),
                _ => false,
            })
        };
        // Blocks the lanes of a console wave reach together. With
        // `wave_flow` every branch is the console wave's decision.
        let whole: Vec<bool> = match &regions {
            _ if st.wave_flow => vec![true; st.exits.len()],
            Some(regions) => {
                let mut w = vec![true; st.exits.len()];
                for (when, region) in regions {
                    if !alike_bool(&same, *when) {
                        for (b, _) in region.iter().enumerate().filter(|r| *r.1) {
                            w[b] = false;
                        }
                    }
                }
                w
            }
            None => vec![false; st.exits.len()],
        };
        let mut next: HashSet<Value> = HashSet::new();
        for b in crate::ir::reverse_postorder(&p.blocks) {
            let block = &p.blocks[b];
            let together = block.preds.iter().all(|&q| whole[q]);
            for phi in &block.phis {
                if matches!(phi.reg, Reg::V(_))
                    && together
                    && phi
                        .args
                        .iter()
                        .all(|a| next.contains(a) || same.contains(a))
                {
                    next.insert(phi.value);
                }
            }
            for (i, inst) in block.insts.iter().enumerate() {
                let known = |x: &Value| next.contains(x) || same.contains(x);
                if st.guards[b][i].is_some_and(|g| !alike_bool(&same, g)) {
                    continue;
                }
                let name = inst.name();
                // The thread id's x shifted past the console wave. A 32-bit
                // shift takes the low five bits of its count (inline 0..64).
                let shifted = match (&inst.fields, wave_tid) {
                    (Fields::Valu(v), Some(tid))
                        if name == "v_lshrrev_b32" && v.dpp.is_none() && v.sdwa.is_none() =>
                    {
                        match v.src[..] {
                            [Operand::Const(k @ 128..=192), Operand::Vector { reg, .. }]
                                if (k - 128) & 31 >= 6 && inst.read(Reg::V(reg)) == Some(tid) =>
                            {
                                Some(tid)
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                // Every VGPR read but the old value of a register the
                // instruction overwrites whole in every lane that runs it.
                let operands = explicit_vgprs(inst);
                let keeps = st.guards[b][i].is_some() || reads_destination(inst);
                let vgprs_alike = inst.reads.iter().all(|&(r, v)| match r {
                    Reg::V(x) => {
                        known(&v)
                            || Some(v) == shifted
                            || (!keeps && inst.write(r).is_some() && !operands.contains(&x))
                    }
                    _ => true,
                });
                let alike = match &inst.fields {
                    Fields::Ds {
                        addr: Some(Operand::Vector { .. }),
                        dst: Some(_),
                        data,
                        gds: false,
                        ..
                    } => name.starts_with("ds_read") && !name.contains("addtid") && data.is_empty(),
                    Fields::Valu(v)
                        if v.dpp.is_none()
                            && v.sdwa.is_none()
                            && !name.starts_with("v_mbcnt")
                            && !name.starts_with("v_cmpx")
                            && !name.starts_with("v_movrel")
                            && !name.starts_with("v_swaprel")
                            && !name.starts_with("v_div_fmas")
                            && !name.contains("lane") =>
                    {
                        // A lane select (a mask's bit per lane): its condition alike.
                        let selects = name == "v_cndmask_b32" || name.contains("_co_ci_");
                        (!selects || v.src.len() > 2)
                            && v.src.iter().enumerate().all(|(k, op)| match *op {
                                Operand::Scalar { code, .. } if selects && k == 2 => inst
                                    .read(Reg::S(code))
                                    .and_then(|m| st.masks.get(&m).or_else(|| st.other.get(&m)))
                                    .is_some_and(|&m| alike_bool(&same, m)),
                                Operand::Const(128 | 193) if selects && k == 2 => true,
                                _ if selects && k == 2 => false,
                                Operand::Scalar { code, .. } if code != NULL => inst
                                    .read(Reg::S(code))
                                    .is_none_or(|x| !derived.contains(&x)),
                                // vccz, execz, scc, lds_direct; attributes
                                Operand::Const(251..=254) | Operand::Attr { .. } => false,
                                _ => true,
                            })
                    }
                    _ => false,
                };
                if vgprs_alike && (alike || shifted.is_some()) {
                    next.extend(inst.writes.iter().filter(|w| w.0 != Reg::Scc).map(|w| w.1));
                }
            }
        }
        if next.is_subset(&same) {
            return same;
        }
        same.extend(next);
    }
}

/// The VGPRs an instruction names as sources (an LDS access: its address and
/// data).
fn explicit_vgprs(inst: &crate::ir::Inst) -> Vec<u8> {
    let ops: Vec<Operand> = match &inst.fields {
        Fields::Valu(v) => v.src.clone(),
        Fields::Ds { addr, data, .. } => addr.iter().chain(data).copied().collect(),
        _ => Vec::new(),
    };
    ops.iter()
        .filter_map(|op| match *op {
            Operand::Vector { reg, dwords } => {
                Some((reg..reg.saturating_add(dwords)).collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// Whether an instruction reads the register it writes beyond its named
/// sources, in lanes that run it: an accumulator, a swap's other side, or the
/// half of a dword a 16-bit result leaves (by name; a wider guess only costs
/// a value counted as alike).
fn reads_destination(inst: &crate::ir::Inst) -> bool {
    let name = inst.name();
    let half = matches!(&inst.fields, Fields::Valu(v) if v.opsel & 8 != 0);
    half || ["mac", "swap", "16", "mix", "dot", "interp"]
        .iter()
        .any(|k| name.contains(k))
}

/// Every per-lane branch, by its condition, with its region: the blocks
/// reachable from it that do not post-dominate it, which only the lanes going
/// one way run. None with an indirect exit (not followed).
fn lane_regions(st: &Structured) -> Option<Vec<(Bool, Vec<bool>)>> {
    let n = st.exits.len();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, e) in st.exits.iter().enumerate() {
        match *e {
            LaneExit::Jump(t) => succ[b].push(t),
            LaneExit::Branch {
                taken, not_taken, ..
            } => succ[b].extend([taken, not_taken]),
            LaneExit::End => {}
            LaneExit::Indirect => return None,
        }
    }
    // Post-dominators, as bit sets; every End leads to one exit.
    let words = n.div_ceil(64);
    let one = |b: usize| {
        let mut v = vec![0u64; words];
        v[b / 64] |= 1 << (b % 64);
        v
    };
    let mut pdom: Vec<Vec<u64>> = (0..n)
        .map(|b| {
            if succ[b].is_empty() {
                one(b)
            } else {
                vec![u64::MAX; words]
            }
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev().filter(|&b| !succ[b].is_empty()) {
            let mut v = vec![u64::MAX; words];
            for &t in &succ[b] {
                for (w, x) in v.iter_mut().zip(&pdom[t]) {
                    *w &= x;
                }
            }
            v[b / 64] |= 1 << (b % 64);
            if v != pdom[b] {
                pdom[b] = v;
                changed = true;
            }
        }
    }
    let mut regions = Vec::new();
    for (d, e) in st.exits.iter().enumerate() {
        let LaneExit::Branch { when, .. } = *e else {
            continue;
        };
        let mut seen = vec![false; n];
        let mut stack = succ[d].clone();
        while let Some(b) = stack.pop() {
            if seen[b] || pdom[d][b / 64] >> (b % 64) & 1 == 1 {
                continue;
            }
            seen[b] = true;
            stack.extend(succ[b].iter().copied());
        }
        regions.push((when, seen));
    }
    Some(regions)
}

#[cfg(test)]
mod tests {
    use crate::ir::{self, Reg};
    use crate::structure;

    const S_ENDPGM: u32 = 0xBF81_0000;

    fn program(words: &[u32]) -> (ir::Program, structure::Structured) {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = ir::build(&code, true, 8).unwrap();
        let st = structure::structure(&p).unwrap();
        (p, st)
    }

    /// `v_lshrrev_b32 v7, k, v0` is alike in a console wave only when every
    /// lane's id shifted by `k`, of which the instruction takes five bits,
    /// is the same.
    #[test]
    fn shifted_thread_ids_are_uniform_only_when_every_lane_agrees() {
        for shift in 0..=64u32 {
            let (p, st) = program(&[0x2C0E_0080 + shift, S_ENDPGM]);
            let inst = &p.blocks[0].insts[0];
            let tid = inst.read(Reg::V(0)).unwrap();
            let shifted = inst.write(Reg::V(7)).unwrap();
            let uniform = super::lane_uniform(&p, &st, Some(tid));
            let expected = (0..64u32).all(|lane| lane.wrapping_shr(shift) == 0);
            assert_eq!(uniform.contains(&shifted), expected, "inline shift {shift}");
        }
    }

    /// A swap writes each side's old value to the other: the thread id it
    /// takes from v0 is not alike, the constant it gives v0 is.
    #[test]
    fn a_swap_also_depends_on_its_old_destination() {
        let (p, st) = program(&[
            0x7E02_0280, // v_mov_b32 v1, 0
            0x7E00_CB01, // v_swap_b32 v0, v1
            S_ENDPGM,
        ]);
        let swap = &p.blocks[0].insts[1];
        assert_eq!(swap.name(), "v_swap_b32");
        let tid = swap.read(Reg::V(0)).unwrap();
        let uniform = super::lane_uniform(&p, &st, Some(tid));
        assert!(uniform.contains(&p.blocks[0].insts[0].write(Reg::V(1)).unwrap()));
        assert!(!uniform.contains(&swap.write(Reg::V(1)).unwrap()));
    }

    /// A select's mask is a bit per lane: a compare of the thread id makes
    /// the selected value differ, a compare of an alike value does not.
    #[test]
    fn a_select_is_alike_only_on_an_alike_mask() {
        for (compare, alike) in [
            (0x7D88_0088, false), // v_cmp_gt_u32 vcc, 8, v0
            (0x7D88_0288, true),  // v_cmp_gt_u32 vcc, 8, v1
        ] {
            let (p, st) = program(&[
                0x7E02_0280, // v_mov_b32 v1, 0
                compare,
                0x0202_0281, // v_cndmask_b32 v1, 1, v1, vcc
                S_ENDPGM,
            ]);
            let insts = &p.blocks[0].insts;
            assert_eq!(insts[2].name(), "v_cndmask_b32");
            let uniform = super::lane_uniform(&p, &st, None);
            let select = insts[2].write(Reg::V(1)).unwrap();
            assert_eq!(uniform.contains(&select), alike, "{compare:#x}");
        }
    }
}
