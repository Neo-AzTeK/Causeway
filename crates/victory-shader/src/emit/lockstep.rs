//! Barriers for groups of one console wave.
//!
//! A wave runs in lockstep: all its lanes are done with one LDS instruction
//! before any starts the next. Where a group is one wave of 64, the console
//! compiler therefore leaves barriers out of code that needs them, and a host
//! that runs the group as two waves of 32 has to have them back.
//!
//! [`barriers`] says where: between LDS accesses whose order matters, in
//! blocks every lane runs exactly once (a barrier some lane does not reach
//! hangs the host). Code between two such blocks (branches, loops) gets none,
//! so its accesses must not depend on one another's order.
// shortcut: a loop's passes are never kept in step, so a program that
// exchanges values through LDS in a loop has no placement; branches that all
// lanes take alike could hold barriers.

use crate::gfx10::Fields;
use crate::ir::{self, Block, Inst};

/// An LDS access, by what its order against others matters to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Access<'a> {
    Read,
    /// An atomic that returns nothing: the same ones commute.
    Atomic(&'a str),
    Write,
}

impl Access<'_> {
    /// Whether lanes may run `self` and `other` in either order.
    fn commutes(self, other: Self) -> bool {
        self == other && self != Access::Write
    }
}

fn access(inst: &Inst) -> Option<Access<'_>> {
    let Fields::Ds { gds: false, .. } = inst.fields else {
        return None;
    };
    let name = inst.name();
    Some(match name {
        "ds_swizzle_b32" => return None,
        _ if name.contains("permute") => return None,
        _ if name.starts_with("ds_read") => Access::Read,
        // A compare with what is there, or an exchange, shows the order.
        _ if name.starts_with("ds_write")
            || name.contains("_rtn")
            || name.contains("cmpst")
            || name.contains("xchg") =>
        {
            Access::Write
        }
        _ => Access::Atomic(name),
    })
}

/// Blocks reachable from `from`'s successors, not through `without`.
fn reach(blocks: &[Block], from: usize, without: Option<usize>) -> Vec<bool> {
    let mut seen = vec![false; blocks.len()];
    let mut todo = blocks[from].exit.successors();
    while let Some(b) = todo.pop() {
        if Some(b) != without && !std::mem::replace(&mut seen[b], true) {
            todo.extend(blocks[b].exit.successors());
        }
    }
    seen
}

/// Where barriers go, as (block, the instruction they precede; its count of
/// instructions: the block's end). None if barriers cannot keep the order.
pub(super) fn barriers(blocks: &[Block]) -> Option<Vec<(usize, usize)>> {
    let order = ir::reverse_postorder(blocks);
    let ends = |b: usize| blocks[b].exit.successors().is_empty();
    // Blocks every lane runs exactly once: on every path to an end, and on
    // no loop.
    let in_loop: Vec<bool> = (0..blocks.len()).map(|b| reach(blocks, b, None)[b]).collect();
    let common = |b: usize| {
        let on_every_path = b == 0 || {
            let around = reach(blocks, 0, Some(b));
            !ends(0) && !(0..blocks.len()).any(|e| around[e] && ends(e))
        };
        on_every_path && !in_loop[b]
    };
    let common: Vec<bool> = (0..blocks.len()).map(common).collect();
    let mut at = Vec::new();
    // Accesses since the last barrier, all of which commute.
    let mut since: Vec<Access> = Vec::new();
    // Where a barrier before the blocks between two common ones goes.
    let mut last = None;
    let commute = |since: &[Access], a: Access| since.iter().all(|&s| s.commutes(a));
    let mut k = 0;
    while k < order.len() {
        let b = order[k];
        if common[b] {
            for (i, inst) in blocks[b].insts.iter().enumerate() {
                if let Some(a) = access(inst) {
                    if !commute(&since, a) {
                        at.push((b, i));
                        since.clear();
                    }
                    since.push(a);
                }
            }
            last = Some((b, blocks[b].insts.len()));
            k += 1;
            continue;
        }
        // Reverse postorder lists every block between two common ones
        // between them.
        let mut between = Vec::new();
        let mut again = false;
        while k < order.len() && !common[order[k]] {
            let before = between.len();
            between.extend(blocks[order[k]].insts.iter().filter_map(access));
            again |= in_loop[order[k]] && between.len() > before;
            k += 1;
        }
        let Some(&first) = between.first() else {
            continue;
        };
        // One write alone has no order, unless a loop repeats it.
        let alone = between.len() == 1 && !again;
        if !alone && !between.iter().all(|&a| first.commutes(a)) {
            return None;
        }
        if !commute(&since, first) {
            at.push(last?);
            since.clear();
        }
        since.push(first);
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDPGM: u32 = 0xBF81_0000;
    const WRITE: [u32; 2] = [0xD834_0000, 0x0000_0100]; // ds_write_b32 v0, v1
    const READ: [u32; 2] = [0xD8D8_0000, 0x0200_0000]; // ds_read_b32 v2, v0
    const MIN: [u32; 2] = [0xD814_0000, 0x0000_0100]; // ds_min_i32 v0, v1

    fn placed(words: &[u32]) -> Option<Vec<(usize, usize)>> {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        barriers(&ir::build(&code, true, 256).unwrap().blocks)
    }

    #[test]
    fn reads_wait_for_writes_and_writes_for_reads() {
        let words = [&WRITE[..], &READ, &READ, &WRITE, &[ENDPGM]].concat();
        assert_eq!(placed(&words), Some(vec![(0, 1), (0, 3)]));
        let words = [&READ[..], &READ, &[ENDPGM]].concat();
        assert_eq!(placed(&words), Some(vec![]));
        // The same atomics commute; a write does not with another.
        let words = [&MIN[..], &MIN, &WRITE, &WRITE, &[ENDPGM]].concat();
        assert_eq!(placed(&words), Some(vec![(0, 2), (0, 3)]));
    }

    #[test]
    fn a_branch_waits_before_it() {
        // Lanes that take the branch skip the read: the barrier precedes it.
        let words = [
            &WRITE[..],
            &[0xBF86_0002], // s_cbranch_vccz -> the end
            &READ,
            &[ENDPGM],
        ]
        .concat();
        let at = placed(&words).unwrap();
        assert_eq!(at.len(), 1);
        assert_eq!(at[0].0, 0);
    }

    #[test]
    fn an_exchange_inside_a_branch_or_loop_has_no_placement() {
        let branch = [&[0xBF86_0004][..], &WRITE, &READ, &[ENDPGM]].concat();
        assert_eq!(placed(&branch), None);
        // One write alone in the branch is fine.
        let write = [&[0xBF86_0002][..], &WRITE, &READ, &[ENDPGM]].concat();
        assert_eq!(placed(&write).map(|at| at.len()), Some(1));
        // s_cbranch_vccz back to the write.
        let looped = [&[0xBF80_0000][..], &WRITE, &[0xBF86_FFFD], &[ENDPGM]].concat();
        assert_eq!(placed(&looped), None);
        // A loop that only reads is fine.
        let reads = [&[0xBF80_0000][..], &WRITE, &READ, &[0xBF86_FFFD], &[ENDPGM]].concat();
        assert!(placed(&reads).is_some());
    }
}
