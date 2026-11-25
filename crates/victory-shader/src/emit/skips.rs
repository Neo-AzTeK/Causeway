//! Skips the console leaves out for two host waves.
//!
//! Compiled code jumps over an `if` whose mask no lane of the wave holds
//! (`s_cbranch_execz`). Where a program of waves of 64 runs as two host waves
//! of 32, each half would decide alone whether to skip, and a half that
//! skipped could not take part in an exchange the other half waits in. The
//! skip only saves work, though: with the branch left out, every lane runs
//! the body under its own EXEC guard, and a lane outside the mask changes
//! nothing there. [`find`] names the skips for which that is so; [`leave_out`]
//! makes them branches to the next instruction, as the IR drops.
//!
//! A skip is left out only if its body is closed (it ends at the skip's
//! target, never at a program end), and no scalar register it writes (EXEC
//! aside) is read after it: that would differ between a skipped and a run
//! body.
// shortcut: scalar loads and lane reads in the body then run with no lane
// enabled; their addresses come from the lanes' old values, and the host
// reads robustly. Only forward skips on EXEC are left out, not those on VCC
// or SCC.

use std::collections::HashSet;

use crate::ir::{EXEC, Exit, Program, Reg, Value};

/// The byte offsets of the `s_cbranch_execz` instructions that may be left out.
pub(super) fn find(p: &Program) -> Vec<usize> {
    let n = p.blocks.len();
    let mut found = Vec::new();
    for (b, block) in p.blocks.iter().enumerate() {
        let Some(last) = block.insts.last() else {
            continue;
        };
        let Exit::Branch { taken, not_taken } = block.exit else {
            continue;
        };
        if last.name() != "s_cbranch_execz"
            || p.blocks[taken].offset <= last.machine.offset
            || taken == b
        {
            continue;
        }
        // The body: what the fallthrough reaches before the target.
        let mut body = vec![false; n];
        let mut todo = vec![not_taken];
        let mut closed = taken != not_taken;
        while let Some(x) = todo.pop() {
            if x == taken || std::mem::replace(&mut body[x], true) {
                continue;
            }
            if x == b || matches!(p.blocks[x].exit, Exit::End | Exit::Indirect) {
                closed = false;
                break;
            }
            todo.extend(p.blocks[x].exit.successors());
        }
        if !closed || body[b] || p.blocks[not_taken].preds.len() != 1 {
            continue;
        }
        if escapes(p, &body) {
            continue;
        }
        found.push(last.machine.offset);
    }
    found
}

/// Whether a scalar value (not EXEC's) a body writes is read outside it.
fn escapes(p: &Program, body: &[bool]) -> bool {
    let mut made: HashSet<Value> = HashSet::new();
    for (b, block) in p.blocks.iter().enumerate().filter(|(b, _)| body[*b]) {
        let _ = b;
        for inst in &block.insts {
            for &(reg, v) in &inst.writes {
                let scalar = match reg {
                    Reg::S(s) => s != EXEC && s != EXEC + 1,
                    Reg::Scc => true,
                    Reg::V(_) => false,
                };
                if scalar {
                    made.insert(v);
                }
            }
        }
        // A merge inside the body of what flows in is no new value.
        for phi in &block.phis {
            if matches!(phi.reg, Reg::S(s) if s != EXEC && s != EXEC + 1) || phi.reg == Reg::Scc {
                made.insert(phi.value);
            }
        }
    }
    // Values read outside the body, through merges: a merge is read if its
    // value is, so follow them to a fixed point.
    let mut read: HashSet<Value> = HashSet::new();
    for (b, block) in p.blocks.iter().enumerate().filter(|(b, _)| !body[*b]) {
        let _ = b;
        for inst in &block.insts {
            read.extend(inst.reads.iter().map(|r| r.1));
        }
    }
    loop {
        let before = read.len();
        for block in &p.blocks {
            for phi in &block.phis {
                if read.contains(&phi.value) {
                    read.extend(phi.args.iter().copied());
                }
            }
        }
        if read.len() == before {
            break;
        }
    }
    made.iter().any(|v| read.contains(v))
}

/// `code` with the instructions at `offsets` (`s_cbranch_*`) jumping to the
/// next instruction.
pub(super) fn leave_out(code: &[u8], offsets: &[usize]) -> Vec<u8> {
    let mut code = code.to_vec();
    for &at in offsets {
        code[at] = 0;
        code[at + 1] = 0;
    }
    code
}
