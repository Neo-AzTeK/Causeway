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
//! wave reach together: past no branch on a condition that is not alike. A
//! value written in a loop counts outside it only if the lanes of a console
//! wave leave it together (every exit reached together, on an alike
//! condition): else each lane carries out its own last iteration's value.
//! Loop-carried values count when what the loop carries is alike; the
//! analysis grows from nothing to a fixed point, so a value that needs itself
//! to be alike never is.
//!
//! Adapted from avatarmanz/VIctory-7900XTX 431cab5, 7348b80 and abbbd6b.
// shortcut: an LDS read at an alike address counts as alike, which holds
// where the two halves read that dword with no write between them; the
// halves are ordered only by the program's barriers (`lockstep` adds more
// for groups of one wave), as for every lane's own read of LDS.

use std::collections::{HashMap, HashSet};

use crate::emit::halves;
use crate::gfx10::{Fields, NULL, Operand};
use crate::io::{Input, Io};
use crate::ir::{Inst, Program, Reg, Value};
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
/// that are the same in every lane of a console wave holding them, where they
/// are read ([`Uniform::alike_at`]).
pub(super) struct Uniform {
    same: HashSet<Value>,
    /// The block each value is defined in.
    def: HashMap<Value, usize>,
    /// Per block: the loops (indices into `apart`) that hold it.
    loops: Vec<Vec<usize>>,
    /// Per loop: lanes of a console wave may leave it at different iterations.
    apart: Vec<bool>,
}

impl Uniform {
    /// Nothing is alike (a host wave of its own, or a flow not followed).
    pub(super) fn none() -> Self {
        Uniform {
            same: HashSet::new(),
            def: HashMap::new(),
            loops: Vec::new(),
            apart: Vec::new(),
        }
    }

    /// Whether `v` is alike where block `at` reads it.
    pub(super) fn alike_at(&self, v: Value, at: usize) -> bool {
        self.same.contains(&v) && self.def.get(&v).is_some_and(|&d| self.left_together(d, at))
    }

    /// A value of block `d` read in block `at`: no loop that holds `d` but
    /// not `at` lets lanes leave at different iterations (each lane would
    /// carry its own last iteration's value out).
    fn left_together(&self, d: usize, at: usize) -> bool {
        let (Some(ld), Some(la)) = (self.loops.get(d), self.loops.get(at)) else {
            return false;
        };
        ld.iter().all(|l| !self.apart[*l] || la.contains(l))
    }
}

/// See [`Uniform`].
pub(super) fn lane_uniform(p: &Program, st: &Structured, wave_tid: Option<Value>) -> Uniform {
    let Some(graph) = Graph::new(st) else {
        return Uniform::none();
    };
    let derived = halves::lane_derived(p);
    let mut def = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        def.extend(block.phis.iter().map(|f| (f.value, b)));
        for inst in &block.insts {
            def.extend(inst.writes.iter().map(|w| (w.1, b)));
        }
    }
    let mut u = Uniform {
        same: HashSet::new(),
        def,
        loops: graph.loops_of.clone(),
        apart: vec![false; graph.loops.len()],
    };
    // A condition read in block `at`.
    let alike_bool = |u: &Uniform, b: Bool, at: usize| {
        st.bools.support(b).iter().all(|a| match *a {
            // An SCC no lane gave is one value in both halves.
            Atom::Uniform(v) => !derived.contains(&v),
            Atom::Lane(v) => u.alike_at(v, at),
            _ => false,
        })
    };
    loop {
        // Blocks the lanes of a console wave reach together, and loops they
        // leave together. With `wave_flow` every branch is the console
        // wave's decision.
        let mut whole = vec![true; st.exits.len()];
        if !st.wave_flow {
            for &(d, ref region) in &graph.regions {
                let LaneExit::Branch { when, .. } = st.exits[d] else {
                    continue;
                };
                if !alike_bool(&u, when, d) {
                    for (b, _) in region.iter().enumerate().filter(|r| *r.1) {
                        whole[b] = false;
                    }
                }
            }
            for (l, body) in graph.loops.iter().enumerate() {
                // Every block with an edge out of the loop: reached by whole
                // console waves, and leaving on an alike condition.
                u.apart[l] = (0..body.len()).filter(|&b| body[b]).any(|b| {
                    let out = graph.succ[b].iter().any(|&t| !body[t]);
                    out && (!whole[b]
                        || matches!(st.exits[b], LaneExit::Branch { when, .. }
                            if !alike_bool(&u, when, b)))
                });
            }
        }
        let mut next: Vec<Value> = Vec::new();
        for b in crate::ir::reverse_postorder(&p.blocks) {
            let block = &p.blocks[b];
            let together = block.preds.iter().all(|&q| whole[q]);
            for phi in &block.phis {
                // Each argument as the phi's block reads it.
                if matches!(phi.reg, Reg::V(_))
                    && together
                    && phi.args.iter().all(|&a| u.alike_at(a, b))
                {
                    next.push(phi.value);
                }
            }
            for (i, inst) in block.insts.iter().enumerate() {
                let guard = st.guards[b][i];
                if guard.is_some_and(|g| !alike_bool(&u, g, b)) {
                    continue;
                }
                let at = Site {
                    b,
                    guarded: guard.is_some(),
                };
                if alike_inst(&u, inst, at, st, &derived, wave_tid, &alike_bool) {
                    next.extend(inst.writes.iter().filter(|w| w.0 != Reg::Scc).map(|w| w.1));
                }
            }
        }
        // `apart` only shrinks as `same` grows: done when nothing is new.
        let mut grown = false;
        for v in next {
            grown |= u.same.insert(v);
        }
        if !grown {
            return u;
        }
    }
}

/// Where an instruction is: its block, and whether it has a lane guard.
#[derive(Clone, Copy)]
struct Site {
    b: usize,
    guarded: bool,
}

/// Whether `inst` writes alike values from what `u` already holds.
fn alike_inst(
    u: &Uniform,
    inst: &Inst,
    at: Site,
    st: &Structured,
    derived: &HashSet<Value>,
    wave_tid: Option<Value>,
    alike_bool: &dyn Fn(&Uniform, Bool, usize) -> bool,
) -> bool {
    let b = at.b;
    let name = inst.name();
    // The thread id's x shifted past the console wave. A 32-bit shift takes
    // the low five bits of its count (inline 0..64).
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
    // Every VGPR read but the old value of a register the instruction
    // overwrites whole in every lane that runs it.
    let operands = explicit_vgprs(inst);
    let keeps = at.guarded || reads_destination(inst);
    let vgprs_alike = inst.reads.iter().all(|&(r, v)| match r {
        Reg::V(x) => {
            u.alike_at(v, b)
                || Some(v) == shifted
                || (!keeps && inst.write(r).is_some() && !operands.contains(&x))
        }
        _ => true,
    });
    if !vgprs_alike {
        return false;
    }
    if shifted.is_some() {
        return true;
    }
    // Every dword of a scalar operand: one no lane gave.
    let scalar_alike = |code: u8, dwords: u8| {
        (code..code.saturating_add(dwords.max(1)))
            .all(|c| inst.read(Reg::S(c)).is_none_or(|x| !derived.contains(&x)))
    };
    match &inst.fields {
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
                        .is_some_and(|&m| alike_bool(u, m, b)),
                    Operand::Const(128 | 193) if selects && k == 2 => true,
                    _ if selects && k == 2 => false,
                    Operand::Scalar { code, dwords } if code != NULL => scalar_alike(code, dwords),
                    // vccz, execz, scc, lds_direct; attributes
                    Operand::Const(251..=254) | Operand::Attr { .. } => false,
                    _ => true,
                })
        }
        _ => false,
    }
}

/// The VGPRs an instruction names as sources (an LDS access: its address and
/// data).
fn explicit_vgprs(inst: &Inst) -> Vec<u8> {
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
fn reads_destination(inst: &Inst) -> bool {
    let name = inst.name();
    let half = matches!(&inst.fields, Fields::Valu(v) if v.opsel & 8 != 0);
    half || ["mac", "swap", "16", "mix", "dot", "interp"]
        .iter()
        .any(|k| name.contains(k))
}

/// The per-lane flow ([`Structured::exits`]) as [`lane_uniform`] reads it.
struct Graph {
    succ: Vec<Vec<usize>>,
    /// Every per-lane branch (its block) with its region: the blocks
    /// reachable from it that do not post-dominate it, which only the lanes
    /// going one way run.
    regions: Vec<(usize, Vec<bool>)>,
    /// Natural loops (one per header), by block.
    loops: Vec<Vec<bool>>,
    /// Per block: the loops that hold it.
    loops_of: Vec<Vec<usize>>,
}

/// A bit set over blocks.
fn set(words: usize, b: usize) -> Vec<u64> {
    let mut v = vec![0u64; words];
    v[b / 64] |= 1 << (b % 64);
    v
}

fn has(v: &[u64], b: usize) -> bool {
    v[b / 64] >> (b % 64) & 1 == 1
}

/// Meets the sets of `from` into one, then adds `b`: a step of a
/// dominator fixed point.
fn meet(sets: &[Vec<u64>], from: &[usize], words: usize, b: usize) -> Vec<u64> {
    let mut v = vec![u64::MAX; words];
    for &t in from {
        for (w, x) in v.iter_mut().zip(&sets[t]) {
            *w &= x;
        }
    }
    v[b / 64] |= 1 << (b % 64);
    v
}

impl Graph {
    /// None with an indirect exit, a block from which no lane ends, or a loop
    /// with two entries (not followed).
    fn new(st: &Structured) -> Option<Graph> {
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
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (b, s) in succ.iter().enumerate() {
            for &t in s {
                pred[t].push(b);
            }
        }
        let words = n.div_ceil(64);
        // Post-dominators; every End leads to one exit.
        let mut pdom: Vec<Vec<u64>> = (0..n)
            .map(|b| {
                if succ[b].is_empty() {
                    set(words, b)
                } else {
                    vec![u64::MAX; words]
                }
            })
            .collect();
        // Dominators from the entry, block 0.
        let mut dom: Vec<Vec<u64>> = (0..n)
            .map(|b| {
                if b == 0 {
                    set(words, 0)
                } else {
                    vec![u64::MAX; words]
                }
            })
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for b in (0..n).rev().filter(|&b| !succ[b].is_empty()) {
                let v = meet(&pdom, &succ[b], words, b);
                if v != pdom[b] {
                    pdom[b] = v;
                    changed = true;
                }
            }
            for b in (1..n).filter(|&b| !pred[b].is_empty()) {
                let v = meet(&dom, &pred[b], words, b);
                if v != dom[b] {
                    dom[b] = v;
                    changed = true;
                }
            }
        }
        // A block from which no lane ends would post-dominate everything.
        let mut ends = vec![false; n];
        let mut todo: Vec<usize> = (0..n).filter(|&b| succ[b].is_empty()).collect();
        while let Some(b) = todo.pop() {
            if !std::mem::replace(&mut ends[b], true) {
                todo.extend(pred[b].iter().copied());
            }
        }
        if ends.contains(&false) {
            return None;
        }
        let mut regions = Vec::new();
        for (d, e) in st.exits.iter().enumerate() {
            if !matches!(e, LaneExit::Branch { .. }) {
                continue;
            }
            let mut seen = vec![false; n];
            let mut stack = succ[d].clone();
            while let Some(b) = stack.pop() {
                if seen[b] || has(&pdom[d], b) {
                    continue;
                }
                seen[b] = true;
                stack.extend(succ[b].iter().copied());
            }
            regions.push((d, seen));
        }
        // Loops: every edge back to a block on the search path must go to a
        // block that dominates its source.
        let mut on_path = vec![false; n];
        let mut visited = vec![false; n];
        let mut headers: Vec<(usize, Vec<usize>)> = Vec::new();
        let mut stack: Vec<(usize, usize)> = Vec::new();
        if n > 0 {
            visited[0] = true;
            on_path[0] = true;
            stack.push((0, 0));
        }
        while let Some(top) = stack.len().checked_sub(1) {
            let (b, k) = stack[top];
            let Some(&t) = succ[b].get(k) else {
                on_path[b] = false;
                stack.pop();
                continue;
            };
            stack[top].1 += 1;
            if on_path[t] {
                if !has(&dom[b], t) {
                    return None;
                }
                match headers.iter_mut().find(|h| h.0 == t) {
                    Some(h) => h.1.push(b),
                    None => headers.push((t, vec![b])),
                }
            } else if !visited[t] {
                visited[t] = true;
                on_path[t] = true;
                stack.push((t, 0));
            }
        }
        let mut loops = Vec::new();
        let mut loops_of = vec![Vec::new(); n];
        for (h, latches) in headers {
            let mut body = vec![false; n];
            body[h] = true;
            let mut todo = latches;
            while let Some(b) = todo.pop() {
                if !std::mem::replace(&mut body[b], true) {
                    todo.extend(pred[b].iter().copied());
                }
            }
            for (b, _) in body.iter().enumerate().filter(|x| *x.1) {
                loops_of[b].push(loops.len());
            }
            loops.push(body);
        }
        Some(Graph {
            succ,
            regions,
            loops,
            loops_of,
        })
    }
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
            assert_eq!(
                uniform.alike_at(shifted, 0),
                expected,
                "inline shift {shift}"
            );
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
        assert!(uniform.alike_at(p.blocks[0].insts[0].write(Reg::V(1)).unwrap(), 0));
        assert!(!uniform.alike_at(swap.write(Reg::V(1)).unwrap(), 0));
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
            assert_eq!(uniform.alike_at(select, 0), alike, "{compare:#x}");
        }
    }

    /// A value written in a loop is alike in each iteration, but lanes that
    /// leave at different iterations carry different ones out: alike after
    /// the loop only where the loop's exit is alike.
    #[test]
    fn a_loop_value_is_alike_after_the_loop_only_if_lanes_leave_together() {
        for (count, together) in [
            (0x3606_0081, false), // v_and_b32 v3, 1, v0
            (0x7E06_0282, true),  // v_mov_b32 v3, 2
        ] {
            let (p, st) = program(&[
                count,
                0x4A06_0681, // v_add_nc_u32 v3, 1, v3
                0xBE82_0380, // s_mov_b32 s2, 0
                0xBE80_047E, // s_mov_b64 s[0:1], exec
                0x7E02_0202, // L: v_mov_b32 v1, s2
                0x8002_8102, // s_add_u32 s2, s2, 1
                0x7DA2_0602, // v_cmpx_lt_u32 s2, v3
                0xBF89_FFFC, // s_cbranch_execnz L
                0xBEFE_0400, // s_mov_b64 exec, s[0:1]
                0x7E04_0301, // v_mov_b32 v2, v1
                S_ENDPGM,
            ]);
            let at = |name: &str, reg: u8| {
                p.blocks
                    .iter()
                    .enumerate()
                    .find_map(|(b, block)| {
                        let i = block
                            .insts
                            .iter()
                            .find(|i| i.name() == name && i.write(Reg::V(reg)).is_some())?;
                        Some((b, i.write(Reg::V(reg)).unwrap()))
                    })
                    .unwrap()
            };
            let (in_loop, v1) = at("v_mov_b32", 1);
            let (after, v2) = at("v_mov_b32", 2);
            assert_ne!(in_loop, after);
            let uniform = super::lane_uniform(&p, &st, None);
            assert!(uniform.alike_at(v1, in_loop), "{count:#x}");
            assert_eq!(uniform.alike_at(v1, after), together, "{count:#x}");
            assert_eq!(uniform.alike_at(v2, after), together, "{count:#x}");
        }
    }
}
