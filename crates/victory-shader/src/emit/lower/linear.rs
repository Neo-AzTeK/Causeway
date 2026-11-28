//! Several console waves in one group, each run as two host waves.
//!
//! The waves of a group may take different sides of a branch, but the host's
//! group barriers need every thread on one path. So the host runs every block,
//! in one topological order, and a console wave that did not choose a block is
//! *off* in it. All a wave has of its own is one bit per edge it took (the
//! same in both its halves): a block's wave is on if it took an edge into it,
//! and a join takes the value of that edge. A value defined in a block that
//! dominates its use needs nothing, as a wave at the use ran the definition.
//! Off waves have no effect: their vector instructions have no lane.
//!
//! Loops (reducible ones) are laid out contiguously, and the whole group goes
//! round again while any of its waves takes a back edge: the one group vote.
//! A header's joins keep their value in a wave that did not take a back edge.
//!
//! An off wave's scalars in a block were never computed, so in a block some
//! wave may be off in, its memory accesses other than LDS (scalar, image,
//! buffer, global) run only where it is on: none of their results would be
//! used, and an address a branch protects may fault. What they write is 0
//! in an off wave.
// shortcut: an off wave's LDS accesses run at whatever address it holds, as
// a lane outside a guard's does; LDS has no fault.

use std::collections::HashMap;

use super::{Lower, Stop, Val, Why, unsupported};
use crate::emit::bitcode::hold::HeldLoop;
use crate::emit::bitcode::{Attr, Bin, Term, V, pred};
use crate::gfx10::Fields;
use crate::ir::{self, Inst, Program};
use crate::native::{NativeProgram, StageIo};
use crate::structure::{LaneExit, Structured};

/// The order the host runs the blocks in, and the loops in it.
struct Layout {
    /// A topological order of the forward edges with every loop contiguous.
    order: Vec<usize>,
    loops: Vec<Loop>,
}

struct Loop {
    header: usize,
    /// By IR block.
    body: Vec<bool>,
}

/// The layout of a program's blocks; `Err` if a loop has two ways in.
fn layout(p: &Program) -> Result<Layout, &'static str> {
    let rpo = ir::reverse_postorder(&p.blocks);
    let mut at = vec![usize::MAX; p.blocks.len()];
    for (n, &b) in rpo.iter().enumerate() {
        at[b] = n;
    }
    let reached = |b: usize| at[b] != usize::MAX;
    let back = |q: usize, h: usize| reached(q) && at[h] <= at[q];
    let mut loops = Vec::new();
    for &h in &rpo {
        let mut work: Vec<usize> = p.blocks[h]
            .preds
            .iter()
            .copied()
            .filter(|&q| back(q, h))
            .collect();
        if work.is_empty() {
            continue;
        }
        let mut body = vec![false; p.blocks.len()];
        body[h] = true;
        while let Some(q) = work.pop() {
            if !reached(q) || std::mem::replace(&mut body[q], true) {
                continue;
            }
            // A way round the header: it does not dominate its latch.
            if q == rpo[0] || at[q] < at[h] {
                return Err("a loop with two ways in, in a group of several waves");
            }
            work.extend(p.blocks[q].preds.iter().copied());
        }
        loops.push(Loop { header: h, body });
    }
    let mut placed = vec![false; p.blocks.len()];
    let mut order: Vec<usize> = Vec::with_capacity(rpo.len());
    // Loops entered and not yet placed whole, the innermost last.
    let mut open: Vec<usize> = Vec::new();
    while order.len() < rpo.len() {
        while let Some(&l) = open.last() {
            let body: &Loop = &loops[l];
            if !rpo.iter().all(|&b| !body.body[b] || placed[b]) {
                break;
            }
            open.pop();
        }
        let next = rpo.iter().copied().find(|&b| {
            !placed[b]
                && open.last().is_none_or(|&l| loops[l].body[b])
                && p.blocks[b]
                    .preds
                    .iter()
                    .all(|&q| !reached(q) || placed[q] || back(q, b))
        });
        let b = next.ok_or("blocks no order of a group of several waves can run")?;
        placed[b] = true;
        order.push(b);
        if let Some(l) = loops.iter().position(|l| l.header == b) {
            open.push(l);
        }
    }
    Ok(Layout { order, loops })
}

pub(crate) struct Linear {
    layout: Layout,
    /// By IR block: some wave of the group may be off in it.
    skipped: Vec<bool>,
    /// By (from, to) block: whether this wave went that way.
    took: HashMap<(usize, usize), V>,
    /// Whether this wave is on in the block being lowered; `None`: all are.
    on: Option<V>,
    /// By loop header: the phi saying whether the wave is on in this pass,
    /// its value on the way in and the DXIL block that comes from.
    header_on: HashMap<usize, (V, V, usize)>,
    /// By block: the DXIL block that goes on to the next one, outside the
    /// loops that end here.
    out: HashMap<usize, usize>,
    /// By loop header: the DXIL block that votes and goes round again.
    ends: HashMap<usize, usize>,
    /// By IR block: whether the wave is on in it.
    ons: HashMap<usize, V>,
    /// By DXIL block: the IR block it is part of; `mark`: the blocks there
    /// were when the one being lowered began.
    owner: HashMap<usize, usize>,
    mark: usize,
}

/// Whether the group of `native` holds several console waves of 64.
fn many_waves(native: &NativeProgram) -> bool {
    matches!(&native.io, StageIo::Cs(cs) if native.wave64
        && cs.threads.iter().product::<u32>() > 64)
}

pub(super) fn new(p: &Program, st: &Structured, native: &NativeProgram) -> Option<Linear> {
    if !(st.wave_flow && many_waves(native)) {
        return None;
    }
    // (A program with no layout is never asked to run this way: `supports`.)
    Some(Linear {
        layout: layout(p).ok()?,
        skipped: crate::emit::lockstep::common(&p.blocks).iter().map(|c| !c).collect(),
        took: HashMap::new(),
        on: None,
        header_on: HashMap::new(),
        out: HashMap::new(),
        ends: HashMap::new(),
        ons: HashMap::new(),
        owner: HashMap::new(),
        mark: 0,
    })
}

/// Whether a program can run this way: `Err` names why not.
pub(crate) fn supports(p: &Program) -> Result<(), &'static str> {
    layout(p).map(|_| ())
}

impl Lower<'_> {
    /// The order blocks are lowered in.
    pub(super) fn block_order(&self) -> Vec<usize> {
        match &self.linear {
            Some(linear) => linear.layout.order.clone(),
            None => ir::reverse_postorder(&self.x.p.blocks),
        }
    }

    /// Whether block `b` heads a loop of a group of several waves.
    pub(super) fn linear_header(&self, b: usize) -> bool {
        self.linear
            .as_ref()
            .is_some_and(|l| l.layout.loops.iter().any(|l| l.header == b))
    }

    /// Before the first block `first`: if it heads a loop, the way into it.
    pub(super) fn linear_start(&mut self, first: usize) {
        if self.linear_header(first) {
            let blk = self.block[first].expect("reachable");
            self.enter_loop(first, first, self.cur);
            self.s.m.terminate(self.cur, Term::Br(blk));
            if let Some(linear) = &mut self.linear {
                linear.out.insert(usize::MAX, self.cur);
            }
        }
    }

    /// The DXIL block that goes on to block `b`.
    fn way_in(&self, b: usize) -> usize {
        let linear = self.linear.as_ref().expect("linear");
        match linear.layout.order[..self.rpo[b]].last() {
            Some(prev) => linear.out[prev],
            // (The prologue's last block, which `linear_start` ended.)
            None => linear.out[&usize::MAX],
        }
    }

    /// The OR of the took bits of the edges into `to` from `from`.
    fn any_took(&mut self, at: usize, from: &[usize], to: usize) -> Option<V> {
        let linear = self.linear.as_ref()?;
        let mut ways: Vec<V> = Vec::new();
        for &q in from {
            if let Some(&t) = linear.took.get(&(q, to))
                && !ways.contains(&t)
            {
                ways.push(t);
            }
        }
        ways.into_iter()
            .reduce(|a, t| self.s.m.bin(at, Bin::Or, a, t))
    }

    /// At the start of block `b` (after its joins): whether the wave is on.
    pub(super) fn linear_enter(&mut self, b: usize) {
        if self.linear.is_none() {
            return;
        }
        let preds = self.x.p.blocks[b].preds.clone();
        let on = if self.linear_header(b) {
            let phi = self.s.m.phi(self.cur, self.s.t.i1);
            let before = self.way_in(b);
            let forward: Vec<usize> = preds
                .into_iter()
                .filter(|&q| self.rpo[q] < self.rpo[b])
                .collect();
            let entered = match self.any_took(before, &forward, b) {
                Some(v) => v,
                None => self.s.m.bool(true),
            };
            if let Some(linear) = &mut self.linear {
                linear.header_on.insert(b, (phi, entered, before));
            }
            Some(phi)
        } else {
            // The entry block has no way in: every wave starts there.
            self.any_took(self.cur, &preds, b)
        };
        let mark = self.s.m.block_count();
        if let Some(linear) = &mut self.linear {
            linear.on = on;
            linear.mark = mark;
            if let Some(on) = on {
                linear.ons.insert(b, on);
            }
        }
    }

    /// The DXIL blocks of IR block `b`, whose lowering has just ended.
    fn own_blocks(&mut self, b: usize) {
        let count = self.s.m.block_count();
        let first = self.block[b].expect("reachable");
        let tail = self.cur;
        if let Some(linear) = &mut self.linear {
            for d in (linear.mark..count).chain([first, tail]) {
                linear.owner.insert(d, b);
            }
        }
    }

    /// With every instruction in place: what a loop computed is held for the
    /// waves that left it.
    pub(super) fn linear_hold(&mut self) -> Result<(), Stop> {
        let Some(linear) = &self.linear else {
            return Ok(());
        };
        let loops: Vec<HeldLoop> = linear
            .ends
            .iter()
            .map(|(&h, &end)| HeldLoop {
                pre: linear.header_on[&h].2,
                header: self.block[h].expect("reachable"),
                end,
            })
            .collect();
        let bits = linear.took.values().chain(linear.ons.values()).copied();
        let on = linear
            .owner
            .iter()
            .filter_map(|(&d, b)| Some((d, *linear.ons.get(b)?)))
            .collect();
        self.s
            .m
            .hold_loop_values(&loops, bits.collect(), &on)
            .map_err(|w| unsupported(0, w))
    }

    /// Ends the loop headed by `h` in `self.cur`: the group goes round again
    /// while any of its waves takes a back edge, up to the iteration limit.
    /// A wave still going round then took no way out: it stays off.
    fn linear_loop_end(&mut self, h: usize) -> Result<(), Why> {
        let end = self.cur;
        let latches: Vec<usize> = self.x.p.blocks[h]
            .preds
            .iter()
            .copied()
            .filter(|&q| self.tail[q].is_some() && self.rpo[q] >= self.rpo[h])
            .collect();
        let again = self
            .any_took(end, &latches, h)
            .ok_or("a loop without a back edge")?;
        let (phi, entered, before) = self.linear.as_ref().expect("linear").header_on[&h];
        self.s.m.set_phi(phi, vec![(entered, before), (again, end)]);
        let any = self.group_vote(again)?;
        let ok = self.count_iteration(h, pred::IULT);
        let repeat = self.s.m.bin(end, Bin::And, any, ok);
        let after = self.s.m.new_block();
        let header = self.block[h].expect("reachable");
        self.s.m.terminate(end, Term::CondBr(repeat, header, after));
        if let Some(linear) = &mut self.linear {
            linear.ends.insert(h, end);
        }
        self.cur = after;
        Ok(())
    }

    /// Whether `mine`, the same in every lane of a console wave, holds in any
    /// wave of the group: a group-shared slot per wave. Every thread runs it.
    fn group_vote(&mut self, mine: V) -> Result<V, Why> {
        let at = self.cur;
        let (_, slot0) = self.halves.ok_or("a group vote")?;
        let [x, y, z] = self.s.threads.ok_or("a group vote")?;
        let waves = (x * y * z).div_ceil(64);
        let (one, zero) = (self.s.m.i32(1), self.s.m.i32(0));
        // Past the pair slots (2 per wave) and the lane slots (64 per wave).
        let first = self.xbase + waves * 66;
        let base = self.s.m.i32(self.xbase);
        let pairs = self.s.m.bin(at, Bin::Sub, slot0, base);
        let wave = self.s.m.bin(at, Bin::LShr, pairs, one);
        let k = self.s.m.i32(first);
        let slot = self.s.m.bin(at, Bin::Add, k, wave);
        let mine = self.s.m.select(at, mine, one, zero);
        let lds = self.s.lds();
        let p = self.s.m.element(at, lds, slot);
        self.s.m.store(at, p, mine);
        let (mode, void) = (self.s.m.i32(1 | 8), self.s.t.void);
        self.s
            .op(at, "barrier", Attr::NoDuplicate, void, 80, &[mode]);
        let mut any = zero;
        for w in 0..waves {
            let slot = self.s.m.i32(first + w);
            let p = self.s.m.element(at, lds, slot);
            let v = self.s.m.load(at, p);
            any = self.s.m.bin(at, Bin::Or, any, v);
        }
        // Reads done before the next vote writes.
        self.s
            .op(at, "barrier", Attr::NoDuplicate, void, 80, &[mode]);
        Ok(self.s.m.cmp(at, pred::NE, any, zero))
    }

    /// An instruction's guard `g`, which holds only in a wave that is on.
    /// Before the instruction with `fields` in block `b`: where some wave
    /// may be off in it and the instruction accesses memory other than LDS,
    /// the code that runs only where this wave is on (module notes); end it
    /// with [`Self::linear_memory_end`].
    pub(super) fn linear_memory(&mut self, b: usize, fields: &Fields) -> Option<(usize, usize)> {
        let memory = matches!(
            fields,
            Fields::Smem { .. } | Fields::Image { .. } | Fields::Buffer { .. } | Fields::Flat { .. }
        );
        let on = self.linear.as_ref().filter(|l| memory && l.skipped[b])?.on?;
        self.begin_guarded(Some(on))
    }

    /// After the instruction `inst` that [`Self::linear_memory`] began:
    /// what it wrote is 0 where this wave is off.
    pub(super) fn linear_memory_end(&mut self, at: Option<(usize, usize)>, inst: &Inst) {
        if at.is_none() {
            return;
        }
        let mut written = Vec::new();
        let mut pairs = Vec::new();
        for &(_, v) in &inst.writes {
            // Kept forms of the value made inside would be used outside it.
            self.floats.remove(&v);
            self.low16.remove(&v);
            let off = match self.vals.get(&v) {
                Some(&Val::I32(x)) => (x, self.s.m.i32(0)),
                Some(&Val::Bit(x)) => (x, self.s.m.bool(false)),
                _ => continue,
            };
            written.push(v);
            pairs.push(off);
        }
        let phis = self.end_guarded(at, &pairs);
        for (v, phi) in written.into_iter().zip(phis) {
            let val = match self.vals[&v] {
                Val::Bit(_) => Val::Bit(phi),
                _ => Val::I32(phi),
            };
            self.vals.insert(v, val);
        }
    }

    pub(super) fn linear_guard(&mut self, g: Option<V>) -> Option<V> {
        let Some(on) = self.linear.as_ref().and_then(|l| l.on) else {
            return g;
        };
        Some(match g {
            Some(g) => self.s.m.bin(self.cur, Bin::And, on, g),
            None => on,
        })
    }

    /// Ends block `b`: the wave notes where it goes, the host goes on to the
    /// next block in order.
    pub(super) fn linear_exit(&mut self, b: usize) -> Result<(), Stop> {
        let offset = self.x.p.blocks[b]
            .insts
            .last()
            .map_or(self.x.p.blocks[b].offset, |i| i.machine.offset);
        let on = self.linear.as_ref().and_then(|l| l.on);
        let ways = match self.x.st.exits[b] {
            LaneExit::Jump(t) => {
                let on = on.unwrap_or_else(|| self.s.m.bool(true));
                vec![(t, on)]
            }
            LaneExit::Branch {
                when,
                taken,
                not_taken,
            } => {
                let at = self.cur;
                let mut c = self.boolean(when, at).map_err(|w| unsupported(offset, w))?;
                if self.wave_decides_branch(when) {
                    c = self
                        .wave_decision(b, c)
                        .map_err(|w| unsupported(offset, w))?;
                }
                if taken == not_taken {
                    let on = on.unwrap_or_else(|| self.s.m.bool(true));
                    vec![(taken, on)]
                } else {
                    let at = self.cur;
                    let on = on.unwrap_or_else(|| self.s.m.bool(true));
                    let yes = self.s.m.bin(at, Bin::And, on, c);
                    // (Not an `xor` of a compare: one hung the AMD driver.)
                    let never = self.s.m.bool(false);
                    let no = self.s.m.select(at, c, never, on);
                    vec![(taken, yes), (not_taken, no)]
                }
            }
            LaneExit::End => Vec::new(),
            LaneExit::Indirect => return Err(unsupported(offset, "an indirect jump")),
        };
        if let Some(linear) = &mut self.linear {
            for (to, bit) in ways {
                linear.took.insert((b, to), bit);
            }
        }
        self.tail[b] = Some(self.cur);
        self.own_blocks(b);
        let linear = self.linear.as_ref().expect("linear");
        let order = &linear.layout.order;
        let n = self.rpo[b];
        let next = order.get(n + 1).copied();
        // The loops whose last block this is, the innermost first.
        let mut ending: Vec<(usize, usize)> = linear
            .layout
            .loops
            .iter()
            .filter(|l| l.body[b] && order[n + 1..].iter().all(|&c| !l.body[c]))
            .map(|l| (self.rpo[l.header], l.header))
            .collect();
        ending.sort_unstable_by(|a, b| b.cmp(a));
        for (_, h) in ending {
            self.linear_loop_end(h)
                .map_err(|w| unsupported(offset, w))?;
        }
        if let Some(linear) = &mut self.linear {
            linear.out.insert(b, self.cur);
        }
        let term = match next {
            Some(next) => {
                if self.linear_header(next) {
                    self.enter_loop(b, next, self.cur);
                }
                Term::Br(self.block[next].expect("reachable"))
            }
            None => Term::Ret,
        };
        self.s.m.terminate(self.cur, term);
        Ok(())
    }

    /// Fills the join `v` of block `b` from `incoming`: (value, the DXIL
    /// block it comes from, the IR block).
    pub(super) fn join(&mut self, v: V, b: usize, incoming: Vec<(V, usize, usize)>) {
        if self.linear.is_none() {
            let incoming = incoming.into_iter().map(|(x, qb, _)| (x, qb)).collect();
            self.s.m.set_phi(v, incoming);
            return;
        }
        let header = self.linear_header(b);
        let before = self.way_in(b);
        let linear = self.linear.as_ref().expect("linear");
        let end = linear.ends.get(&b).copied();
        // The ways in from before the block, then a header's back edges.
        let (mut forward, mut back) = (Vec::new(), Vec::new());
        for (x, _, q) in incoming {
            let way = (x, linear.took.get(&(q, b)).copied());
            if header && self.rpo[q] >= self.rpo[b] {
                back.push(way);
            } else {
                forward.push(way);
            }
        }
        // (A header no edge from before reaches has no value there yet.)
        let mut x = forward.first().or(back.first()).expect("a way in").0;
        for &(y, took) in forward.iter().skip(1) {
            if let Some(took) = took {
                x = self.s.m.select(before, took, y, x);
            }
        }
        let mut incoming = vec![(x, before)];
        if let Some(end) = end {
            // A wave that did not go round keeps the value.
            let mut next = v;
            for (y, took) in back {
                if let Some(took) = took {
                    next = self.s.m.select(end, took, y, next);
                }
            }
            incoming.push((next, end));
        }
        self.s.m.set_phi(v, incoming);
    }
}
