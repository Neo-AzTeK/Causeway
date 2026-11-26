//! Whether a program of waves of 64 may run as two host waves of 32.
//!
//! On the console, scalars (SGPR, SCC, VCC and EXEC as 64-bit masks) are one
//! per wave. Run as two halves, a scalar derived from the lanes (a compare
//! mask, EXEC) is per half, unless the lowering exchanges it. A branch on such
//! a scalar can take one half out of a loop or past a block while the other
//! half stays, and an `s_barrier` that only one half reaches never completes.
//! [`check`] finds that case from the program alone; the lowering's own
//! exchanges (lane reads, ballots) only run where every lane arrives once, so
//! they are not a hazard here.
//!
//! A scalar is lane-derived if a vector instruction wrote it (a compare, a
//! carry-out; not the lane reads, which the lowering exchanges), if it is the
//! entry EXEC, or if a scalar instruction computed it from one.
// shortcut: lane-derived is a property of the whole value, not of its bits; a
// loop on a lane-derived scalar with no `s_barrier` it controls is accepted
// (each half iterates alone, and the lowering refuses an exchange inside it).

use std::collections::HashSet;

use crate::gfx10::Fields;
use crate::ir::{EXEC, Exit, Inst, Program, Reg, Value};

fn scalar(reg: Reg) -> bool {
    matches!(reg, Reg::S(_) | Reg::Scc)
}

/// The scalar values derived from lanes.
fn lane_derived(p: &Program) -> HashSet<Value> {
    let mut set: HashSet<Value> = p
        .inputs
        .iter()
        .filter(|(r, _)| matches!(r, Reg::S(s) if *s == EXEC || *s == EXEC + 1))
        .map(|&(_, v)| v)
        .collect();
    loop {
        let before = set.len();
        for block in &p.blocks {
            for phi in &block.phis {
                if scalar(phi.reg) && phi.args.iter().any(|a| set.contains(a)) {
                    set.insert(phi.value);
                }
            }
            for inst in &block.insts {
                let from_lanes = match inst.fields {
                    Fields::Valu(_) => !matches!(
                        inst.name(),
                        "v_readlane_b32" | "v_readfirstlane_b32"
                    ),
                    _ => inst.reads.iter().any(|r| scalar(r.0) && set.contains(&r.1)),
                };
                if from_lanes {
                    set.extend(inst.writes.iter().filter(|w| scalar(w.0)).map(|w| w.1));
                }
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

fn reads_lanes(inst: &Inst, set: &HashSet<Value>) -> bool {
    inst.reads.iter().any(|r| scalar(r.0) && set.contains(&r.1))
}

/// Blocks reachable from `from`'s successors, not through `without`.
fn reach(p: &Program, from: usize, without: usize) -> Vec<bool> {
    let mut seen = vec![false; p.blocks.len()];
    let mut todo = p.blocks[from].exit.successors();
    while let Some(b) = todo.pop() {
        if b != without && !std::mem::replace(&mut seen[b], true) {
            todo.extend(p.blocks[b].exit.successors());
        }
    }
    seen
}

/// Whether the halves of every wave of 64 stay in step: `Err` names why not.
pub(super) fn check(p: &Program) -> Result<(), &'static str> {
    let set = lane_derived(p);
    scalars(p, &set, true)?;
    barriers(p, &set)
}

/// The part of [`check`] that holds even where both halves take every branch
/// together. Scalar loads are not part of it: there a mask read as a number
/// is the ballot of both halves, so an address computed from one is the
/// console's in each.
pub(super) fn check_together(p: &Program) -> Result<(), &'static str> {
    scalars(p, &lane_derived(p), false)
}

fn scalars(p: &Program, set: &HashSet<Value>, loads: bool) -> Result<(), &'static str> {
    // With `loads`, scalar memory is addressed, and EXEC is set, by a lane-derived scalar
    // other than a lane-wise mask operation.
    for block in &p.blocks {
        for inst in &block.insts {
            if !reads_lanes(inst, set) {
                continue;
            }
            if loads && matches!(inst.fields, Fields::Smem { .. }) {
                return Err("a scalar load addressed by a lane-derived scalar");
            }
            let name = inst.name();
            let to_exec = inst
                .writes
                .iter()
                .any(|w| matches!(w.0, Reg::S(s) if s == EXEC || s == EXEC + 1));
            let mask_op = ["s_and_", "s_or_", "s_xor_", "s_andn2_", "s_orn2_", "s_nand_", "s_nor_"]
                .iter()
                .chain(&["s_xnor_", "s_not_", "s_mov_", "s_wqm_"])
                .any(|p| name.starts_with(p))
                || name.contains("saveexec");
            if to_exec && !mask_op && matches!(inst.fields, Fields::Sop { .. }) {
                return Err("EXEC set from a lane-derived scalar by arithmetic");
            }
        }
    }
    Ok(())
}

fn barriers(p: &Program, set: &HashSet<Value>) -> Result<(), &'static str> {
    let n = p.blocks.len();
    let barriers: Vec<usize> = (0..n)
        .filter(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "s_barrier"))
        .collect();
    if barriers.is_empty() {
        return Ok(());
    }
    for c in 0..n {
        let Exit::Branch { .. } = p.blocks[c].exit else {
            continue;
        };
        if !p.blocks[c].insts.last().is_some_and(|i| reads_lanes(i, set)) {
            continue;
        }
        for &b in &barriers {
            let from_c = reach(p, c, usize::MAX);
            if !from_c[b] {
                continue;
            }
            // A path from the branch that skips the barrier, or goes round a
            // loop the barrier is in without it, makes one half run it a
            // different number of times.
            let avoid = reach(p, c, b);
            let ends = (0..n).any(|e| avoid[e] && p.blocks[e].exit.successors().is_empty());
            let in_loop = from_c[c] && reach(p, b, usize::MAX)[c];
            if ends || in_loop && avoid[c] {
                return Err("an s_barrier that a lane-derived branch may skip or repeat");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir;

    const ENDPGM: u32 = 0xBF81_0000;
    const BARRIER: u32 = 0xBF8A_0000;
    const CMP: u32 = 0x7D88_0088; // v_cmp_gt_u32 vcc_lo, 8, v0
    const VCCNZ_BACK: u32 = 0xBF87_FFFD; // s_cbranch_vccnz back over 3 words
    const NOP: u32 = 0xBF80_0000;

    fn verdict(words: &[u32]) -> Result<(), &'static str> {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        check(&ir::build(&code, true, 256).unwrap())
    }

    #[test]
    fn a_barrier_in_a_loop_with_a_lane_derived_exit_is_refused() {
        // 1: barrier; 2: v_cmp; 3: s_cbranch_vccnz 1 (a loop cannot enter at 0)
        assert!(verdict(&[NOP, BARRIER, CMP, VCCNZ_BACK, ENDPGM]).is_err());
    }

    #[test]
    fn a_barrier_in_a_loop_with_a_uniform_exit_is_accepted() {
        // VCC is never written: it is the same in both halves.
        assert_eq!(verdict(&[NOP, BARRIER, NOP, VCCNZ_BACK, ENDPGM]), Ok(()));
    }

    #[test]
    fn a_lane_derived_loop_without_a_barrier_is_accepted() {
        assert_eq!(verdict(&[NOP, NOP, CMP, VCCNZ_BACK, ENDPGM]), Ok(()));
    }

    #[test]
    fn a_barrier_after_a_lane_derived_loop_is_accepted() {
        assert_eq!(verdict(&[NOP, NOP, CMP, VCCNZ_BACK, BARRIER, ENDPGM]), Ok(()));
    }

    #[test]
    fn a_barrier_a_lane_derived_branch_skips_is_refused() {
        // v_cmp; s_cbranch_vccz -> end; s_barrier; end
        assert!(verdict(&[CMP, 0xBF86_0001, BARRIER, ENDPGM]).is_err());
    }
}
