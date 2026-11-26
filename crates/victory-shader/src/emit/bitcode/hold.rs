//! Holding what a loop computed for the console waves that left it, where the
//! host goes round it for the whole group (`lower::linear`).
//!
//! This works on the finished instructions, so nothing a later pass or a
//! cache computed from a loop's values can be missed. A loop here is laid out
//! as: the block that goes into its header, the blocks of a pass, and the one
//! block (`end`) that goes round or leaves, which every pass runs.

use std::collections::{HashMap, HashSet};

use super::{Bin, Module, Op, Term, Ty, TypeDef, V};

/// A loop of DXIL blocks every thread runs in step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeldLoop {
    /// The block that goes into the loop.
    pub pre: usize,
    pub header: usize,
    /// The block that goes back to `header` or out.
    pub end: usize,
}

struct Holder<'a> {
    loops: &'a [HeldLoop],
    /// By loop, the innermost first: whether each block is in it.
    bodies: Vec<Vec<bool>>,
    /// By instruction: its block.
    def: HashMap<u32, usize>,
    /// Bits that say what a wave did in one pass: false in a wave that is
    /// off, so over a loop they are kept by `or`.
    bits: HashSet<V>,
    /// By block: whether the wave is on in it.
    on: &'a HashMap<usize, V>,
    /// By held value: what is true if the wave computed it.
    on_of: HashMap<V, V>,
    held: HashMap<(V, usize), V>,
    /// By held value: the loop it is already held over.
    over: HashMap<V, usize>,
}

impl Module {
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Makes every use outside a loop of `loops` of a value computed in it
    /// read what the value was in the last pass in which the wave was on in
    /// its block (`on`, by DXIL block). A bit of `bits` reads whether any
    /// pass of this run of the loop set it. `Err` where DXIL cannot hold the
    /// value.
    pub fn hold_loop_values(
        &mut self,
        loops: &[HeldLoop],
        bits: HashSet<V>,
        on: &HashMap<usize, V>,
    ) -> Result<(), &'static str> {
        if loops.is_empty() {
            return Ok(());
        }
        let mut preds = vec![Vec::new(); self.blocks.len()];
        for (b, block) in self.blocks.iter().enumerate() {
            match block.term {
                Some(Term::Br(t)) => preds[t].push(b),
                Some(Term::CondBr(_, t, f)) => {
                    preds[t].push(b);
                    preds[f].push(b);
                }
                Some(Term::Ret) | None => {}
            }
        }
        let body = |l: &HeldLoop| {
            let mut inside = vec![false; preds.len()];
            inside[l.header] = true;
            let mut work = vec![l.end];
            while let Some(b) = work.pop() {
                if !std::mem::replace(&mut inside[b], true) {
                    work.extend(&preds[b]);
                }
            }
            inside
        };
        let mut sized: Vec<(HeldLoop, Vec<bool>)> = loops.iter().map(|l| (*l, body(l))).collect();
        sized.sort_by_key(|(_, b)| b.iter().filter(|&&x| x).count());
        let (loops, bodies): (Vec<_>, Vec<_>) = sized.into_iter().unzip();
        // What is there before any value is held.
        let mut uses: Vec<(usize, u32)> = Vec::new();
        let mut def = HashMap::new();
        for (b, block) in self.blocks.iter().enumerate() {
            for &i in block.phis.iter().chain(&block.body) {
                def.insert(i, b);
                uses.push((b, i));
            }
        }
        let terms: Vec<(usize, V, usize, usize)> = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(b, block)| match block.term {
                Some(Term::CondBr(c, t, f)) => Some((b, c, t, f)),
                _ => None,
            })
            .collect();
        let mut h = Holder {
            loops: &loops,
            bodies,
            def,
            bits,
            on,
            on_of: HashMap::new(),
            held: HashMap::new(),
            over: HashMap::new(),
        };
        for (b, i) in uses {
            let mut op = self.insts[i as usize].op.clone();
            if let Op::Phi(incoming) = &mut op {
                for (v, from) in incoming {
                    *v = h.read(self, *v, *from)?;
                }
            } else {
                for v in operands(&mut op) {
                    *v = h.read(self, *v, b)?;
                }
            }
            self.insts[i as usize].op = op;
        }
        for (b, c, t, f) in terms {
            let c = h.read(self, c, b)?;
            self.blocks[b].term = Some(Term::CondBr(c, t, f));
        }
        Ok(())
    }
}

fn operands(op: &mut Op) -> Vec<&mut V> {
    match op {
        Op::Bin(_, a, b) | Op::Cmp(_, a, b) | Op::Store(a, b) | Op::AtomicRmw(_, a, b) => {
            vec![a, b]
        }
        Op::Select(a, b, c) | Op::CmpXchg(a, b, c) => vec![a, b, c],
        Op::Cast(_, a) | Op::Extract(a, _) | Op::Load(a) => vec![a],
        Op::Call(_, args) => args.iter_mut().collect(),
        Op::Gep(_, a, rest) => std::iter::once(a).chain(rest.iter_mut()).collect(),
        Op::Phi(incoming) => incoming.iter_mut().map(|(v, _)| v).collect(),
    }
}

impl Holder<'_> {
    /// `v` as block `at` reads it: held over each loop that has its block and
    /// not `at`, the innermost first.
    fn read(&mut self, m: &mut Module, v: V, at: usize) -> Result<V, &'static str> {
        let V::Inst(i) = v else { return Ok(v) };
        let from = self.def[&i];
        let first = self.over.get(&v).map_or(0, |&l| l + 1);
        match (first..self.loops.len()).find(|&l| self.bodies[l][from] && !self.bodies[l][at]) {
            Some(l) => {
                let held = self.hold(m, v, l)?;
                self.read(m, held, at)
            }
            None => Ok(v),
        }
    }

    /// A phi of loop `l` and what its end makes of it with `next`.
    fn carry(
        &mut self,
        m: &mut Module,
        l: usize,
        ty: Ty,
        zero: V,
        next: impl FnOnce(&mut Module, usize, V) -> V,
    ) -> V {
        let HeldLoop { pre, header, end } = self.loops[l];
        let phi = m.phi(header, ty);
        let out = next(m, end, phi);
        m.set_phi(phi, vec![(zero, pre), (out, end)]);
        for (v, b) in [(phi, header), (out, end)] {
            if let V::Inst(i) = v {
                self.def.insert(i, b);
            }
        }
        out
    }

    /// The value `v` of a block of loop `l`, as the end of the loop has it.
    fn hold(&mut self, m: &mut Module, v: V, l: usize) -> Result<V, &'static str> {
        if let Some(&x) = self.held.get(&(v, l)) {
            return Ok(x);
        }
        let end = self.loops[l].end;
        let ty = m.type_of(v);
        let at_end = self.read(m, v, end)?;
        let out = if self.bits.contains(&v) {
            let never = m.bool(false);
            let out = self.carry(m, l, ty, never, |m, end, phi| {
                m.bin(end, Bin::Or, phi, at_end)
            });
            self.bits.insert(out);
            out
        } else {
            let zero = match m.types[ty.0 as usize] {
                TypeDef::Int(_) => m.int_const(ty, 0),
                TypeDef::Float => m.f32(0),
                _ => return Err("a handle or address of a loop in a group of several waves"),
            };
            let wrote = self.written(v)?;
            let wrote_at_end = self.read(m, wrote, end)?;
            let out = self.carry(m, l, ty, zero, |m, end, phi| {
                m.select(end, wrote_at_end, at_end, phi)
            });
            // Held further out, it is new where this run of the loop wrote.
            self.bits.insert(wrote);
            let ran = self.hold(m, wrote, l)?;
            self.on_of.insert(out, ran);
            out
        };
        self.over.insert(out, l);
        self.held.insert((v, l), out);
        Ok(out)
    }

    /// What is true if the wave computed `v` in this pass.
    fn written(&self, v: V) -> Result<V, &'static str> {
        if let Some(&w) = self.on_of.get(&v) {
            return Ok(w);
        }
        let V::Inst(i) = v else {
            return Err("a held value with no block");
        };
        self.on
            .get(&self.def[&i])
            .copied()
            .ok_or("a value of a loop from a block of no wave")
    }
}
