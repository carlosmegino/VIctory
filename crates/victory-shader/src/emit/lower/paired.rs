//! Compute-only paired-lane lowering adapted from Pilote Production's
//! GPL-3.0 implementation in fxceless/victory-fork#854, commit 26600c6.
//! One host wave executes one console wave, two logical lanes per thread.

use super::*;
use crate::emit::bitcode::Rmw;

#[derive(Default)]
pub(super) struct LaneState {
    vals: HashMap<Value, Val>,
    floats: HashMap<Value, V>,
    low16: HashMap<Value, V>,
    bools: HashMap<(usize, Bool), V>,
    lane_bits: HashMap<Value, V>,
    lane_fail: HashMap<Value, Why>,
    mask_phis: HashMap<Value, V>,
    pending_masks: Vec<(V, usize, usize)>,
    data_phis: Vec<(V, usize, usize)>,
    read_phis: HashSet<Value>,
    handles: HashMap<(usize, Site, u8, bool, u32), V>,
    join_handles: HashMap<(String, bool, u32), V>,
}

pub(super) fn check_control_flow(p: &Program, st: &Structured) -> Result<(), Why> {
    for (block, exit) in st.exits.iter().enumerate() {
        if let LaneExit::Branch { when, .. } = *exit
            && st.wave_branches[block].is_none()
            && st
                .bools
                .support(when)
                .iter()
                .any(|a| !matches!(a, Atom::Uniform(_)))
        {
            return Err("a branch without wave-uniform control in paired compute");
        }
    }
    check_barriers(p)
}

/// All guest barriers must be at the same acyclic sites on every complete
/// path. The loop budget takes a conditional latch's exit. An unconditional
/// latch can return early, bypassing a group barrier.
fn check_barriers(p: &Program) -> Result<(), Why> {
    let order = crate::ir::reverse_postorder(&p.blocks);
    let rank: HashMap<_, _> = order
        .iter()
        .copied()
        .enumerate()
        .map(|(i, b)| (b, i))
        .collect();
    let barriers: Vec<_> = order
        .iter()
        .copied()
        .filter(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "s_barrier"))
        .collect();
    if barriers.is_empty() {
        return Ok(());
    }
    for &b in &order {
        let back_edges = p.blocks[b]
            .exit
            .successors()
            .into_iter()
            .filter(|s| rank[s] <= rank[&b])
            .count();
        if back_edges == 2 || back_edges == 1 && matches!(p.blocks[b].exit, Exit::Jump(_)) {
            return Err("a loop that can abort before a paired group barrier");
        }
    }
    for barrier in barriers {
        let mut seen = HashSet::new();
        let mut work = p.blocks[barrier].exit.successors();
        while let Some(b) = work.pop() {
            if b == barrier {
                return Err("a paired group barrier in a loop");
            }
            if seen.insert(b) {
                work.extend(p.blocks[b].exit.successors());
            }
        }
        seen.clear();
        work.push(order[0]);
        while let Some(b) = work.pop() {
            if b == barrier || !seen.insert(b) {
                continue;
            }
            let next = p.blocks[b].exit.successors();
            if next.is_empty() {
                return Err("a path that can bypass a paired group barrier");
            }
            work.extend(next);
        }
    }
    Ok(())
}

impl<'a> Lower<'a> {
    pub(super) fn guest_lds_dwords(&self) -> u32 {
        if self.twin.is_some()
            && let StageIo::Cs(cs) = self.x.native.io
        {
            return cs.lds_bytes() / 4;
        }
        self.s.lds_dwords
    }

    pub(super) fn swap_lanes(&mut self) {
        let Some(t) = self.twin.as_deref_mut() else {
            return;
        };
        std::mem::swap(&mut self.vals, &mut t.vals);
        std::mem::swap(&mut self.floats, &mut t.floats);
        std::mem::swap(&mut self.low16, &mut t.low16);
        std::mem::swap(&mut self.bools, &mut t.bools);
        std::mem::swap(&mut self.lane_bits, &mut t.lane_bits);
        std::mem::swap(&mut self.lane_fail, &mut t.lane_fail);
        std::mem::swap(&mut self.mask_phis, &mut t.mask_phis);
        std::mem::swap(&mut self.pending_masks, &mut t.pending_masks);
        std::mem::swap(&mut self.data_phis, &mut t.data_phis);
        std::mem::swap(&mut self.read_phis, &mut t.read_phis);
        std::mem::swap(&mut self.handles, &mut t.handles);
        std::mem::swap(&mut self.join_handles, &mut t.join_handles);
        self.upper = !self.upper;
    }

    /// Evaluate the two lanes in guest order and restore the caller's lane.
    pub(super) fn both<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Result<T, Why>,
    ) -> Result<[T; 2], Why> {
        let upper = self.upper;
        if upper {
            self.swap_lanes();
        }
        let lo = f(self);
        self.swap_lanes();
        let hi = f(self);
        if !upper {
            self.swap_lanes();
        }
        Ok([lo?, hi?])
    }

    fn mirror(&mut self, inst: &Inst) {
        let t = self.twin.as_deref_mut().expect("paired lanes");
        for &(_, v) in &inst.writes {
            if let Some(&value) = self.vals.get(&v) {
                t.vals.insert(v, value);
            }
        }
    }

    /// Each full physical wave reserves one logical wave ID. Extra LDS
    /// lies past the guest allocation, even on a host that schedules
    /// physical waves in a different order.
    fn paired_wave_base(&mut self) -> V {
        if let Some(base) = self.wave_base {
            return base;
        }
        let [threads, _, _] = self.s.threads.expect("compute group");
        let zero = self.s.m.i32(0);
        let base = if threads == 32 {
            zero
        } else {
            let e = self.s.entry;
            let lds = self.s.lds();
            let slot = self.s.m.i32(self.s.lds_dwords - 1);
            let ptr = self.s.m.element(e, lds, slot);
            self.s.m.atomic(e, Rmw::Xchg, ptr, zero);
            let mode = self.s.m.i32(1 | 8);
            self.s
                .op(e, "barrier", Attr::NoDuplicate, self.s.t.void, 80, &[mode]);
            let lane = self.lane_index();
            let leader = self.s.m.cmp(e, pred::EQ, lane, zero);
            let wave = self.s.m.i32(64);
            let add = self.s.m.select(e, leader, wave, zero);
            let old = self.s.m.atomic(e, Rmw::Add, ptr, add);
            self.s.op(
                e,
                "waveReadLaneFirst.i32",
                Attr::NoUnwind,
                self.s.t.i32,
                118,
                &[old],
            )
        };
        self.wave_base = Some(base);
        base
    }

    pub(super) fn paired_lane(&mut self, at: usize) -> V {
        let lane = self.s.op(
            at,
            "waveGetLaneIndex",
            Attr::ReadOnly,
            self.s.t.i32,
            111,
            &[],
        );
        let half = self.s.m.i32(if self.upper { 32 } else { 0 });
        self.s.m.bin(at, Bin::Add, lane, half)
    }

    pub(super) fn paired_thread(&mut self, c: u8) -> Val {
        let StageIo::Cs(cs) = self.x.native.io else {
            unreachable!()
        };
        let [x, y, _] = cs.threads;
        let base = self.paired_wave_base();
        let e = self.s.entry;
        let lane = self.paired_lane(e);
        let f = self.s.m.bin(e, Bin::Add, base, lane);
        let (x, y, xy) = (self.s.m.i32(x), self.s.m.i32(y), self.s.m.i32(x * y));
        Val::I32(match c {
            0 => self.s.m.bin(e, Bin::URem, f, x),
            1 => {
                let row = self.s.m.bin(e, Bin::UDiv, f, x);
                self.s.m.bin(e, Bin::URem, row, y)
            }
            _ => self.s.m.bin(e, Bin::UDiv, f, xy),
        })
    }

    pub(super) fn paired_vote(&mut self, when: Bool, at: usize, any: bool) -> Result<V, Why> {
        let [lo, hi] = self.both(|l| l.boolean(when, at))?;
        let pair = self
            .s
            .m
            .bin(at, if any { Bin::Or } else { Bin::And }, lo, hi);
        let (name, opcode) = if any {
            ("waveAnyTrue", 113)
        } else {
            ("waveAllTrue", 114)
        };
        self.s.flags |= flags::WAVE_OPS;
        Ok(self
            .s
            .op(at, name, Attr::NoUnwind, self.s.t.i1, opcode, &[pair]))
    }

    /// Masked election in one physical wave. A safe lane index is passed to
    /// WaveReadLaneAt even when no lane in the half has EXEC set.
    fn paired_first(&mut self, x: V, guard: Option<V>) -> (V, V) {
        let c = self.cur;
        let ty = self.s.t.i32;
        let first = self
            .s
            .op(c, "waveReadLaneFirst.i32", Attr::NoUnwind, ty, 118, &[x]);
        let Some(guard) = guard else {
            return (first, self.s.m.bool(true));
        };
        let four = self.s.m.named_struct("dx.types.fouri32", vec![ty; 4]);
        let ballot = self
            .s
            .op(c, "waveActiveBallot", Attr::NoUnwind, four, 116, &[guard]);
        let bits = self.s.m.extract(c, ballot, 0);
        let zero = self.s.m.i32(0);
        let any = self.s.m.cmp(c, pred::NE, bits, zero);
        let low = self
            .s
            .op(c, "unaryBits.i32", Attr::ReadNone, ty, 32, &[bits]);
        let lane = self.s.m.select(c, any, low, zero);
        let read = self.read_lane(x, lane);
        (self.s.m.select(c, any, read, first), any)
    }

    fn paired_read_lane(&mut self, inst: &Inst, b: usize, i: usize) -> Result<(), Why> {
        self.cross_lane(false)?;
        let c = self.cur;
        let r = if inst.name() == "v_readfirstlane_b32" {
            let [(lo, in_lo), (hi, in_hi)] = self.both(|l| {
                let x = l.src(inst, 0)?;
                let guard = l.guard(b, i)?;
                Ok(l.paired_first(x, guard))
            })?;
            let hi = self.s.m.select(c, in_hi, hi, lo);
            self.s.m.select(c, in_lo, lo, hi)
        } else {
            let index = self.src(inst, 1)?;
            let [lo, hi] = self.both(|l| l.src(inst, 0))?;
            let (k31, k32, zero) = (self.s.m.i32(31), self.s.m.i32(32), self.s.m.i32(0));
            let physical = self.s.m.bin(c, Bin::And, index, k31);
            let upper_bit = self.s.m.bin(c, Bin::And, index, k32);
            let upper = self.s.m.cmp(c, pred::NE, upper_bit, zero);
            let (lo, hi) = (self.read_lane(lo, physical), self.read_lane(hi, physical));
            self.s.m.select(c, upper, hi, lo)
        };
        self.write_i(inst, r, None)?;
        self.mirror(inst);
        Ok(())
    }

    pub(super) fn paired_inst(&mut self, b: usize, i: usize) -> Result<(), Stop> {
        let inst = &self.x.p.blocks[b].insts[i];
        let name = inst.name();
        if matches!(name, "v_readlane_b32" | "v_readfirstlane_b32") {
            return self
                .paired_read_lane(inst, b, i)
                .map_err(|why| unsupported(inst.machine.offset, why));
        }
        if matches!(inst.fields, Fields::Sopp { .. })
            || matches!(inst.fields, Fields::Smem { .. })
                && (name.contains("store") || name.contains("atomic"))
        {
            self.inst(b, i)?;
            self.mirror(inst);
            return Ok(());
        }
        if matches!(inst.fields, Fields::Ds { .. })
            && !name.starts_with("ds_swizzle")
            && !name.contains("permute")
        {
            // The physical wave issues each guest LDS operation in order.
            // Fence LDS between guest instructions, without synchronizing
            // independently progressing waves in the thread group.
            let mode = self.s.m.i32(8);
            self.s.op(
                self.cur,
                "barrier",
                Attr::NoDuplicate,
                self.s.t.void,
                80,
                &[mode],
            );
        }
        self.inst(b, i)?;
        self.swap_lanes();
        let r = self.inst(b, i);
        self.swap_lanes();
        r
    }
}
