//! Whether a lane that enters a block over one edge ever uses what the
//! block's phis take on that edge.
//!
//! The console compiler reuses registers freely: where paths join, an SGPR
//! may hold a number on one and a descriptor's dword or a lane mask on
//! another, and the code that reads the number runs only for lanes from the
//! first (a flag constant on each path guards it). The dword has no host
//! value, so the emitter may put anything in its place exactly if no effect
//! of the lane depends on it.
//!
//! [`unread`] follows the lane from the edge: constants moved, integer
//! compares of them and the lane booleans they decide fold branches, and
//! everything computed from the dword is tainted. A tainted operand of
//! anything other lanes or memory see (a store, an export, a cross-lane
//! operation, an implicit derivative), a tainted branch, or a tainted guard
//! of such an instruction ends the proof. The rest of the state is taken to
//! be arbitrary, so proofs for several edges hold together.

use std::collections::{HashMap, HashSet};

use super::lower::{Parts, reads_mask_as_data};
use crate::gfx10::{Fields, NULL, Operand};
use crate::ir::{EXEC, Inst, Reg, Value};
use crate::structure::{Atom, Bool, LaneExit};

/// A boolean of the lane: known, or tainted. (Absent: any value, clean.)
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bit {
    Known(bool),
    Tainted,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    known: HashMap<Value, u32>,
    tainted: HashSet<Value>,
    /// Compare and carry results, SCC, mask joins: what [`Atom`]s name.
    bits: HashMap<Value, Bit>,
}

impl State {
    /// Joins `other` in; whether that changed anything.
    fn join(&mut self, other: &State) -> bool {
        let before = self.clone();
        self.known.retain(|v, x| other.known.get(v) == Some(x));
        self.tainted.extend(&other.tainted);
        self.bits
            .retain(|v, b| *b == Bit::Tainted || other.bits.get(v) == Some(&*b));
        for (&v, &b) in &other.bits {
            if b == Bit::Tainted {
                self.bits.insert(v, b);
            }
        }
        *self != before
    }

    fn set_bit(&mut self, v: Value, b: Option<Bit>) {
        match b {
            Some(b) => self.bits.insert(v, b),
            None => self.bits.remove(&v),
        };
    }
}

struct Walk<'a, 'b> {
    x: &'b Parts<'a>,
    /// A lane mask's high dword, by its low one.
    high: &'b dyn Fn(Value) -> Option<Value>,
    /// Blocks with whole-wave sections.
    sections: &'b dyn Fn(usize) -> bool,
    defs: HashMap<Value, (usize, usize)>,
}

/// Whether no effect of a lane entering block `to` over predecessor `n`
/// depends on the phis that `missing` picks there.
pub(super) fn unread(
    x: &Parts,
    to: usize,
    n: usize,
    missing: &dyn Fn(Value) -> bool,
    high: &dyn Fn(Value) -> Option<Value>,
    sections: &dyn Fn(usize) -> bool,
) -> bool {
    let mut defs = HashMap::new();
    for (b, block) in x.p.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            for &(_, v) in &inst.writes {
                defs.insert(v, (b, i));
            }
        }
    }
    Walk {
        x,
        high,
        sections,
        defs,
    }
    .run(to, n, missing)
    .is_some()
}

impl Walk<'_, '_> {
    fn run(&self, to: usize, n: usize, missing: &dyn Fn(Value) -> bool) -> Option<()> {
        let blocks = &self.x.p.blocks;
        let mut before = State::default();
        self.learn(to, n, &mut before);
        let mut at_entry: Vec<Option<State>> = vec![None; blocks.len()];
        let first = self.enter(&before, to, n, missing);
        at_entry[to] = Some(first);
        let mut work = vec![to];
        // Each block's state only loses knowledge; the limit bounds the cost.
        let mut steps = 16 * blocks.len();
        while let Some(b) = work.pop() {
            steps = steps.checked_sub(1)?;
            let mut s = at_entry[b].clone()?;
            if s.tainted.is_empty() && !s.bits.values().any(|b| *b == Bit::Tainted) {
                continue;
            }
            if (self.sections)(b) {
                return None;
            }
            for i in 0..blocks[b].insts.len() {
                self.step(&mut s, b, i)?;
            }
            let next = match self.x.st.exits[b] {
                LaneExit::Jump(t) => vec![t],
                LaneExit::Branch {
                    when,
                    taken,
                    not_taken,
                } => match self.boolean(&s, when) {
                    Some(Bit::Known(true)) => vec![taken],
                    Some(Bit::Known(false)) => vec![not_taken],
                    Some(Bit::Tainted) => return None,
                    None => vec![taken, not_taken],
                },
                LaneExit::End => vec![],
                LaneExit::Indirect => return None,
            };
            for t in next {
                for (n, _) in blocks[t].preds.iter().enumerate().filter(|p| *p.1 == b) {
                    let s = self.enter(&s, t, n, &|_| false);
                    let changed = match &mut at_entry[t] {
                        Some(old) => old.join(&s),
                        none => {
                            *none = Some(s);
                            true
                        }
                    };
                    if changed && !work.contains(&t) {
                        work.push(t);
                    }
                }
            }
        }
        Some(())
    }

    /// What the branches on the way to predecessor `n` of `to` decided: up
    /// to the first join, where each tests one atom.
    fn learn(&self, to: usize, n: usize, s: &mut State) {
        let blocks = &self.x.p.blocks;
        let (mut b, mut via) = (to, n);
        let mut seen = HashSet::new();
        while let Some(&q) = blocks[b].preds.get(via) {
            if !seen.insert(q) {
                break;
            }
            if let LaneExit::Branch {
                when,
                taken,
                not_taken,
            } = self.x.st.exits[q]
                && taken != not_taken
                && let Some((Atom::Lane(v) | Atom::Uniform(v) | Atom::Phi(v), lo, hi)) =
                    self.x.st.bools.node(when)
                && let Some(value) = match (lo, hi) {
                    (Bool::FALSE, Bool::TRUE) => Some(taken == b),
                    (Bool::TRUE, Bool::FALSE) => Some(taken != b),
                    _ => None,
                }
            {
                s.bits.entry(v).or_insert(Bit::Known(value));
            }
            if blocks[q].preds.len() != 1 {
                break;
            }
            (b, via) = (q, 0);
        }
    }

    /// The state in `to` after its phis, from `s` at the end of its
    /// predecessor `n`.
    fn enter(&self, s: &State, to: usize, n: usize, missing: &dyn Fn(Value) -> bool) -> State {
        let mut out = s.clone();
        for phi in &self.x.p.blocks[to].phis {
            let (v, a) = (phi.value, phi.args[n]);
            out.known.remove(&v);
            out.tainted.remove(&v);
            if missing(a) {
                out.tainted.insert(v);
                out.set_bit(v, Some(Bit::Tainted));
                continue;
            }
            if let Some(k) = self.constant(s, a) {
                out.known.insert(v, k);
            }
            if s.tainted.contains(&a) {
                out.tainted.insert(v);
            }
            out.set_bit(v, self.bit_of(s, a));
        }
        // A missing mask or SCC argument still joins as a lane boolean.
        for phi in &self.x.p.blocks[to].phis {
            let a = phi.args[n];
            if missing(a) && phi.reg != Reg::Scc && self.x.st.masks.contains_key(&a) {
                out.set_bit(phi.value, self.bit_of(s, a));
            }
        }
        out
    }

    /// The boolean a value carries: a lane mask's bit, else SCC's.
    fn bit_of(&self, s: &State, v: Value) -> Option<Bit> {
        match self.x.st.masks.get(&v) {
            Some(&m) => self.boolean(s, m),
            None => s.bits.get(&v).copied(),
        }
    }

    fn boolean(&self, s: &State, b: Bool) -> Option<Bit> {
        self.decide(s, b, &mut HashMap::new())
    }

    fn decide(
        &self,
        s: &State,
        b: Bool,
        memo: &mut HashMap<Bool, Option<Bit>>,
    ) -> Option<Bit> {
        if b == Bool::TRUE || b == Bool::FALSE {
            return Some(Bit::Known(b == Bool::TRUE));
        }
        if let Some(&r) = memo.get(&b) {
            return r;
        }
        let (atom, lo, hi) = self.x.st.bools.node(b)?;
        let a = match atom {
            Atom::Lane(v) | Atom::Phi(v) => s.bits.get(&v).copied(),
            Atom::Uniform(v) => match s.bits.get(&v) {
                Some(&b) => Some(b),
                None if s.tainted.contains(&v) => Some(Bit::Tainted),
                None => None,
            },
            Atom::Quad(_) => Some(Bit::Known(true)),
            Atom::Opaque(v) => {
                let taints = |v: Value| s.tainted.contains(&v);
                (taints(v) || (self.high)(v).is_some_and(taints)).then_some(Bit::Tainted)
            }
            Atom::Constant(_) => None,
        };
        let r = match a {
            Some(Bit::Known(k)) => self.decide(s, if k { hi } else { lo }, memo),
            _ => {
                let (l, h) = (self.decide(s, lo, memo), self.decide(s, hi, memo));
                match (l, h) {
                    (Some(Bit::Known(x)), Some(Bit::Known(y))) if x == y => l,
                    _ if a == Some(Bit::Tainted)
                        || l == Some(Bit::Tainted)
                        || h == Some(Bit::Tainted) =>
                    {
                        Some(Bit::Tainted)
                    }
                    _ => None,
                }
            }
        };
        memo.insert(b, r);
        r
    }

    /// A value known on this path, or everywhere (moves of constants).
    fn constant(&self, s: &State, v: Value) -> Option<u32> {
        if let Some(&k) = s.known.get(&v) {
            return Some(k);
        }
        let &(b, i) = self.defs.get(&v)?;
        let inst = &self.x.p.blocks[b].insts[i];
        let op = match (&inst.fields, inst.name()) {
            (Fields::Sop { src, .. }, "s_mov_b32") => src.first(),
            (Fields::Valu(f), "v_mov_b32") if plain(inst) && self.x.st.guards[b][i].is_none() => {
                f.src.first()
            }
            _ => None,
        };
        match *op? {
            Operand::Literal(x) => Some(x),
            Operand::Const(c) => crate::resource::inline(c),
            _ => None,
        }
    }

    fn value(&self, s: &State, inst: &Inst, op: &Operand) -> Option<u32> {
        match *op {
            Operand::Literal(x) => Some(x),
            Operand::Const(c) => crate::resource::inline(c),
            Operand::Scalar { code: NULL, .. } => Some(0),
            Operand::Scalar { code, .. } => self.constant(s, inst.read(Reg::S(code))?),
            Operand::Vector { reg, .. } => self.constant(s, inst.read(Reg::V(reg))?),
            Operand::Attr { .. } => None,
        }
    }

    /// Runs instruction `i` of block `b`; `None` ends the proof.
    fn step(&self, s: &mut State, b: usize, i: usize) -> Option<()> {
        let inst = &self.x.p.blocks[b].insts[i];
        let name = inst.name();
        let sop = matches!(inst.fields, Fields::Sop { .. });
        // Reading a mask as a number reads its lane's bit too.
        let taints = |v: Value| {
            let mask = match self.x.st.highs.get(&v) {
                Some(lo) => lo,
                None => &v,
            };
            s.tainted.contains(&v)
                || self.x.st.masks.contains_key(mask)
                    && self.bit_of(s, *mask) == Some(Bit::Tainted)
                || !self.x.st.masks.contains_key(mask)
                    && s.bits.get(&v) == Some(&Bit::Tainted)
        };
        // EXEC of a vector instruction is its guard.
        let reads = |r: &(Reg, Value)| sop || !matches!(r.0, Reg::S(c) if c == EXEC || c == EXEC + 1);
        let mut tainted = inst.reads.iter().filter(|r| reads(r)).any(|r| taints(r.1));
        let guard = match self.x.st.guards[b][i] {
            None => Some(Bit::Known(true)),
            Some(g) => self.boolean(s, g),
        };
        let waterfall = &self.x.res.waterfall;
        let own = waterfall.compares.contains(&(b, i))
            || inst.writes.iter().any(|w| waterfall.reads.contains(&w.1));
        let cross = !own
            && (name.contains("lane")
                || name.contains("permute")
                || name.contains("mbcnt")
                || name.starts_with("ds_swizzle")
                || matches!(&inst.fields, Fields::Valu(v) if v.dpp.is_some())
                || reads_mask_as_data(self.x.st, inst));
        // A descriptor or its index may have to be the same in every lane.
        let uniform = matches!(
            inst.fields,
            Fields::Image { .. } | Fields::Buffer { .. } | Fields::Smem { .. }
        ) && inst
            .reads
            .iter()
            .any(|r| matches!(r.0, Reg::S(c) if c != EXEC && c != EXEC + 1) && taints(r.1));
        // The host takes the level from the quad's coordinates.
        let derivative = matches!(inst.fields, Fields::Image { .. })
            && ["image_sample", "image_gather4", "image_get_lod"]
                .iter()
                .any(|p| name.starts_with(p))
            && !name.split('_').any(|t| matches!(t, "lz" | "l" | "d" | "cd"));
        let memory = matches!(
            inst.fields,
            Fields::Image { .. } | Fields::Buffer { .. } | Fields::Ds { .. } | Fields::Flat { .. }
        );
        let effect = matches!(inst.fields, Fields::Exp { .. } | Fields::Sopp { .. })
            && !name.starts_with("s_cbranch")
            || name.contains("store")
            || name.contains("atomic")
            || matches!(inst.fields, Fields::Ds { .. }) && !name.starts_with("ds_read")
            || matches!(inst.fields, Fields::Flat { .. }) && !name.contains("_load_");
        if uniform
            || cross && (tainted || guard == Some(Bit::Tainted))
            || (effect || derivative)
                && (tainted || guard == Some(Bit::Tainted))
                && !(memory && effect && guard == Some(Bit::Known(false)))
        {
            return None;
        }

        // A select takes one operand where the lane's bit is known.
        let mut result = None;
        if let Fields::Valu(v) = &inst.fields {
            if name == "v_cndmask_b32"
                && v.sdwa.is_none()
                && let Some(&Operand::Scalar { code, .. }) = v.src.get(2)
                && let Some(m) = inst.read(Reg::S(code))
                && let Some(&m) = self.x.st.masks.get(&m)
            {
                let of = |s: &State, k: usize| match v.src.get(k) {
                    Some(Operand::Vector { reg, .. }) => inst
                        .read(Reg::V(*reg))
                        .is_some_and(|v| s.tainted.contains(&v)),
                    Some(Operand::Scalar { code, .. }) if *code != NULL => {
                        inst.read(Reg::S(*code)).is_some_and(taints)
                    }
                    _ => false,
                };
                let bit = self.boolean(s, m);
                let old = v.dst.is_some_and(|d| match d {
                    Operand::Vector { reg, .. } => inst
                        .read(Reg::V(reg))
                        .is_some_and(|v| s.tainted.contains(&v)),
                    _ => false,
                });
                tainted = match bit {
                    Some(Bit::Tainted) => true,
                    Some(Bit::Known(k)) => of(s, usize::from(k)),
                    None => of(s, 0) || of(s, 1),
                } || old && guard != Some(Bit::Known(true));
                if plain(inst)
                    && let Some(Bit::Known(k)) = bit
                {
                    result = v.src.get(usize::from(k)).and_then(|o| self.value(s, inst, o));
                }
            } else if name == "v_mov_b32" && plain(inst) {
                result = v.src.first().and_then(|o| self.value(s, inst, o));
            }
        } else if let Fields::Sop { src, .. } = &inst.fields {
            result = match (name, &src[..]) {
                ("s_mov_b32", [a]) => self.value(s, inst, a),
                ("s_cselect_b32", [a, c]) => match inst.read(Reg::Scc).map(|v| s.bits.get(&v)) {
                    Some(Some(Bit::Known(true))) => self.value(s, inst, a),
                    Some(Some(Bit::Known(false))) => self.value(s, inst, c),
                    _ => None,
                },
                _ => None,
            };
        }
        // Compares of known integers.
        let compare = |src: &[Operand], op: &str| {
            let [a, c, ..] = src else { return None };
            int_compare(op, self.value(s, inst, a)?, self.value(s, inst, c)?)
        };
        let bit = match &inst.fields {
            Fields::Valu(_) if waterfall.compares.contains(&(b, i)) => Some(Bit::Known(true)),
            Fields::Valu(v) if plain(inst) => name
                .strip_prefix("v_cmp_")
                .or(name.strip_prefix("v_cmpx_"))
                .and_then(|op| compare(&v.src, op))
                .map(Bit::Known),
            Fields::Sop { src, .. } => name
                .strip_prefix("s_cmp_")
                .and_then(|op| compare(src, op))
                .map(Bit::Known),
            _ => None,
        };
        let taint = |v: Value, s: &mut State| {
            s.known.remove(&v);
            s.tainted.remove(&v);
            if tainted {
                s.tainted.insert(v);
            }
        };
        for &(reg, w) in &inst.writes {
            match reg {
                // A vector write outside EXEC keeps the old value.
                Reg::V(_) if guard == Some(Bit::Known(false)) => {
                    let old = inst.read(reg);
                    let known = old.and_then(|o| self.constant(s, o));
                    let bad = old.is_some_and(|o| s.tainted.contains(&o));
                    s.known.remove(&w);
                    s.tainted.remove(&w);
                    if bad {
                        s.tainted.insert(w);
                    }
                    if let Some(k) = known {
                        s.known.insert(w, k);
                    }
                }
                Reg::V(_) => {
                    taint(w, s);
                    if guard == Some(Bit::Tainted) {
                        s.tainted.insert(w);
                    } else if let Some(k) = result
                        && guard == Some(Bit::Known(true))
                        && !tainted
                    {
                        s.known.insert(w, k);
                    }
                }
                Reg::S(_) | Reg::Scc => {
                    taint(w, s);
                    if let Some(k) = result
                        && reg != Reg::Scc
                        && sop
                        && !tainted
                    {
                        s.known.insert(w, k);
                    }
                    let b = if tainted { Some(Bit::Tainted) } else { bit };
                    s.set_bit(w, b);
                }
            }
        }
        Some(())
    }
}

/// A vector instruction without modifiers.
fn plain(inst: &Inst) -> bool {
    match &inst.fields {
        Fields::Valu(v) => {
            (v.neg, v.abs, v.neg_hi, v.opsel, v.omod) == (0, 0, 0, 0, 0)
                && !v.clamp
                && v.sdwa.is_none()
                && v.dpp.is_none()
        }
        _ => false,
    }
}

/// An integer compare's result, by the end of its name.
fn int_compare(op: &str, a: u32, b: u32) -> Option<bool> {
    let (rel, ty) = op.split_once('_')?;
    let order = match ty {
        "u32" => a.cmp(&b),
        "i32" => (a as i32).cmp(&(b as i32)),
        _ => return None,
    };
    Some(match rel {
        "eq" => order.is_eq(),
        "ne" | "lg" => order.is_ne(),
        "lt" => order.is_lt(),
        "le" => order.is_le(),
        "gt" => order.is_gt(),
        "ge" => order.is_ge(),
        _ => return None,
    })
}
