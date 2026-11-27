//! Conservative, explicit prototype for a wave-private no-return LDS counter.
//!
//! Not enabled in normal translation. A proof covers every LDS access before
//! publication, not just the atomic's address. Unknown values lose precision;
//! they never become evidence of independence. No guest input is rewritten by
//! the tests before this analysis runs.
//!
//! The analysis and guest rewrite depend only on native/IR definitions, not on
//! paired lowering. Applying it to another backend still requires validating
//! that backend's scalar/EXEC semantics and synchronization costs.

use crate::gfx10::{Fields, Operand};
use crate::ir::{Exit, Inst, Program, Reg};
use crate::native::{NativeProgram, StageIo};

type Word = Option<u32>;

struct State {
    // A scalar is replicated for the present lanes of its logical wave.
    s: Vec<Vec<Word>>,
    v: Vec<Vec<Word>>,
    threads: usize,
}

impl State {
    fn new(threads: usize) -> Self {
        let mut out = Self {
            s: vec![vec![None; threads]; 128],
            v: vec![vec![None; threads]; 256],
            threads,
        };
        out.v[0] = (0..threads).map(|t| Some(t as u32)).collect();
        for t in 0..threads {
            let count = (threads - t / 64 * 64).min(64);
            let mask = u64::MAX >> (64 - count);
            out.s[126][t] = Some(mask as u32);
            out.s[127][t] = Some((mask >> 32) as u32);
        }
        out
    }

    fn operand(&self, op: Operand, t: usize) -> Word {
        match op {
            Operand::Vector { reg, dwords: 1 } => self.v[reg as usize][t],
            Operand::Scalar { code: 125, .. } => Some(0),
            Operand::Scalar { code, .. } => self.s.get(code as usize)?[t],
            Operand::Const(c @ 128..=192) => Some(u32::from(c - 128)),
            Operand::Const(c @ 193..=208) => Some((192i32 - i32::from(c)) as u32),
            Operand::Literal(x) => Some(x),
            _ => None,
        }
    }

    fn active(&self, t: usize) -> Option<bool> {
        Some(self.s[126 + (t % 64) / 32][t]? & (1 << (t % 32)) != 0)
    }

    fn full(&self) -> bool {
        (0..self.threads).step_by(64).all(|t| {
            let count = (self.threads - t).min(64);
            let present = u64::MAX >> (64 - count);
            self.s[126][t]
                .zip(self.s[127][t])
                .map(|(lo, hi)| u64::from(lo) | u64::from(hi) << 32)
                == Some(present)
        })
    }

    fn forget(&mut self, reg: Reg) {
        match reg {
            Reg::S(r) => self.s[r as usize].fill(None),
            Reg::V(r) => self.v[r as usize].fill(None),
            Reg::Scc => {}
        }
    }

    fn step(&mut self, inst: &Inst) {
        let mut values = Vec::new();
        match &inst.fields {
            Fields::Sop {
                dst: Some(Operand::Scalar { code, dwords }),
                src,
                ..
            } if matches!(inst.name(), "s_mov_b32" | "s_mov_b64") => {
                for word in 0..*dwords {
                    let op = match src[0] {
                        Operand::Scalar {
                            code: crate::gfx10::NULL,
                            ..
                        } => Operand::Literal(0),
                        Operand::Scalar { code, dwords } => Operand::Scalar {
                            code: code + word,
                            dwords,
                        },
                        op if word == 0 => op,
                        Operand::Const(193..=208) => Operand::Literal(u32::MAX),
                        Operand::Const(128..=192) | Operand::Literal(0) => Operand::Literal(0),
                        _ => Operand::Const(255),
                    };
                    values.push((
                        Reg::S(code + word),
                        (0..self.threads).map(|t| self.operand(op, t)).collect(),
                    ));
                }
            }
            Fields::Valu(v)
                if v.dpp.is_none()
                    && v.sdwa.is_none()
                    && v.neg == 0
                    && v.abs == 0
                    && v.omod == 0
                    && !v.clamp
                    && v.opsel == 0
                    && v.opsel_hi == 0
                    && v.neg_hi == 0 =>
            {
                if let Some(Operand::Vector { reg, dwords: 1 }) = v.dst {
                    let next = (0..self.threads)
                        .map(|t| {
                            let arg = |n| self.operand(*v.src.get(n)?, t);
                            let result = match inst.name() {
                                "v_mov_b32" => arg(0),
                                "v_and_b32" => Some(arg(0)? & arg(1)?),
                                "v_xor_b32" => Some(arg(0)? ^ arg(1)?),
                                "v_or_b32" => Some(arg(0)? | arg(1)?),
                                "v_lshrrev_b32" => Some(arg(1)? >> (arg(0)? & 31)),
                                "v_lshlrev_b32" => Some(arg(1)? << (arg(0)? & 31)),
                                "v_add_nc_u32" => Some(arg(0)?.wrapping_add(arg(1)?)),
                                "v_sub_nc_u32" => Some(arg(0)?.wrapping_sub(arg(1)?)),
                                _ => None,
                            };
                            match self.active(t) {
                                Some(true) => result,
                                Some(false) => self.v[reg as usize][t],
                                None if result == self.v[reg as usize][t] => result,
                                None => None,
                            }
                        })
                        .collect();
                    values.push((Reg::V(reg), next));
                } else if inst.name() == "v_cmpx_eq_u32" {
                    // Exact only when every source and incoming EXEC bit is known.
                    let mut masks = vec![None; self.threads];
                    for start in (0..self.threads).step_by(64) {
                        let end = (start + 64).min(self.threads);
                        let bits = (start..end).try_fold(0u64, |bits, t| {
                            let live = self.active(t)?;
                            let equal = self.operand(v.src[0], t)? == self.operand(v.src[1], t)?;
                            Some(bits | (u64::from(live && equal) << (t - start)))
                        });
                        masks[start..end].fill(bits);
                    }
                    for r in [106, 107, 126, 127] {
                        values.push((
                            Reg::S(r),
                            masks
                                .iter()
                                .map(|m| m.map(|m| (m >> ((r & 1) * 32)) as u32))
                                .collect(),
                        ));
                    }
                }
            }
            _ => {}
        }
        for &(r, _) in &inst.writes {
            self.forget(r);
        }
        for (r, v) in values {
            if inst.write(r).is_none() {
                continue;
            }
            match r {
                Reg::S(r) => self.s[r as usize] = v,
                Reg::V(r) => self.v[r as usize] = v,
                Reg::Scc => {}
            }
        }
    }

    fn addresses(&self, inst: &Inst) -> Result<Vec<u32>, &'static str> {
        let Fields::Ds {
            addr: Some(addr),
            offset0,
            offset1,
            gds: false,
            ..
        } = inst.fields
        else {
            return Err("unknown LDS address or GDS");
        };
        let offset = u32::from(offset0) | u32::from(offset1) << 8;
        (0..self.threads)
            .map(|t| {
                self.operand(addr, t)
                    .and_then(|a| a.checked_add(offset))
                    .ok_or("unknown or wrapping LDS address")
            })
            .collect()
    }
}

pub(super) struct Promotion {
    pub native: NativeProgram,
    pub counter_addresses: Vec<u32>,
    pub atomic_offset: usize,
    pub publication_offset: usize,
    pub scalar_words: u8,
    pub vector_words: u16,
}

fn ds(inst: &Inst) -> bool {
    matches!(inst.fields, Fields::Ds { .. })
}

fn ordinary(inst: &Inst) -> bool {
    match &inst.fields {
        Fields::Sop { .. } => matches!(
            inst.name(),
            "s_mov_b32" | "s_mov_b64" | "s_and_b64" | "s_or_b64" | "s_xor_b64"
        ),
        Fields::Sopp { .. } => matches!(
            inst.name(),
            "s_cbranch_execnz" | "s_cbranch_scc1" | "s_endpgm"
        ),
        Fields::Valu(v) => {
            v.dpp.is_none()
                && v.sdwa.is_none()
                && v.neg == 0
                && v.abs == 0
                && v.neg_hi == 0
                && v.opsel == 0
                && v.opsel_hi == 0
                && v.omod == 0
                && !v.clamp
                && matches!(
                    inst.name(),
                    "v_mov_b32"
                        | "v_and_b32"
                        | "v_xor_b32"
                        | "v_or_b32"
                        | "v_lshrrev_b32"
                        | "v_lshlrev_b32"
                        | "v_add_nc_u32"
                        | "v_sub_nc_u32"
                        | "v_readfirstlane_b32"
                        | "v_cmp_gt_u32"
                        | "v_cmp_ne_u32"
                        | "v_cmpx_eq_u32"
                        | "v_cmpx_lt_u32"
                )
        }
        _ => false,
    }
}

/// Require SCC to be killed before any use on either successor path. Added
/// arithmetic must not change an original scalar branch or select's condition.
fn scc_dead_after(p: &Program, block: usize, index: usize) -> bool {
    let mut pending = vec![(block, index + 1)];
    let mut seen = std::collections::HashSet::new();
    while let Some((b, at)) = pending.pop() {
        if !seen.insert((b, at)) {
            continue;
        }
        let mut killed = false;
        for i in &p.blocks[b].insts[at..] {
            if i.read(Reg::Scc).is_some() {
                return false;
            }
            if i.write(Reg::Scc).is_some() {
                killed = true;
                break;
            }
        }
        if !killed {
            pending.extend(p.blocks[b].exit.successors().into_iter().map(|b| (b, 0)));
        }
    }
    true
}

/// Supported shape: a linear initializer, one independently looping wave
/// region, then a linear publication phase with a group barrier before peer
/// observations. Different loop visit counts and arbitrary shrinking EXEC are
/// allowed. Returning atomics, multiple entries, unknown aliases, indirect
/// register writes, GDS and memory-to-LDS instructions are refused.
pub(super) fn promote(native: &NativeProgram, p: &Program) -> Result<Promotion, &'static str> {
    let StageIo::Cs(cs) = native.io else {
        return Err("not compute");
    };
    if !native.wave64
        || cs.threads[1..] != [1, 1]
        || !(1..=1024).contains(&cs.threads[0])
        || native.vgprs >= 256
    {
        return Err("unsupported thread layout or register allocation");
    }
    if p.blocks.len() != 3
        || p.blocks[0].exit != Exit::Jump(1)
        || p.blocks[1].exit
            != (Exit::Branch {
                taken: 1,
                not_taken: 2,
            })
        || p.blocks[2].exit != Exit::End
        || !p.blocks[0].preds.is_empty()
        || p.blocks[1].preds != [0, 1]
    {
        return Err("counter region is not a single-entry loop followed by linear publication");
    }
    if p.blocks.iter().flat_map(|b| &b.insts).any(|i| {
        matches!(
            i.fields,
            Fields::Buffer { lds: true, .. } | Fields::Flat { lds: true, .. }
        )
    }) {
        return Err("untracked memory-to-LDS effect");
    }
    if p.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .flat_map(|i| i.reads.iter().chain(&i.writes))
        .any(|(r, _)| matches!(r, Reg::S(128..)))
    {
        return Err("parked scalar registers need a separate allocation proof");
    }
    if p.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .flat_map(|i| i.reads.iter().chain(&i.writes))
        .any(|(reg, _)| matches!(reg, Reg::V(r) if u16::from(*r) >= native.vgprs))
    {
        return Err("guest vector operand exceeds its declared register allocation");
    }
    let loop_block = &p.blocks[1];
    let additions: Vec<_> = loop_block
        .insts
        .iter()
        .enumerate()
        .filter(|(_, i)| ds(i))
        .collect();
    let [(add_index, add)] = additions.as_slice() else {
        return Err("loop must contain exactly one LDS operation");
    };
    if add.name() != "ds_add_u32"
        || !matches!(
            add.fields,
            Fields::Ds {
                dst: None,
                gds: false,
                ..
            }
        )
        || loop_block.insts.iter().any(|i| !ds(i) && !ordinary(i))
    {
        return Err(
            "loop contains an observable atomic result or another memory/rendezvous effect",
        );
    }
    if !scc_dead_after(p, 1, *add_index) {
        return Err("atomic replacement would clobber live SCC");
    }
    let mut state = State::new(cs.threads[0] as usize);
    let stores: Vec<_> = p.blocks[0].insts.iter().filter(|i| ds(i)).collect();
    let [init] = stores.as_slice() else {
        return Err("one proven counter initializer required");
    };
    let mut initialized = false;
    let mut addresses = Vec::new();
    for i in &p.blocks[0].insts {
        if ds(i) {
            if i.name() != "ds_write_b32" {
                return Err("initializer has another LDS effect");
            }
            addresses = state.addresses(i)?;
            let Fields::Ds { data, .. } = &i.fields else {
                unreachable!()
            };
            for t in 0..state.threads {
                if state.active(t) != Some(t % 64 == 0)
                    || (t % 64 == 0 && state.operand(data[0], t) != Some(0))
                {
                    return Err("initializer must write zero exactly once per wave");
                }
            }
            initialized = true;
        } else if !ordinary(i) {
            return Err("untracked initializer effect");
        }
        state.step(i);
    }
    if !initialized || !state.full() {
        return Err("loop entry must restore all present lanes");
    }
    // Population count has no per-lane effect guard. It must never count a
    // missing lane, even if later stores would be guarded by the paired tail.
    // This first version permits only intersections with the current EXEC in
    // the loop, which also covers lanes permanently leaving on different visits.
    for i in &loop_block.insts {
        if i.writes.iter().any(|(r, _)| matches!(r, Reg::S(126 | 127))) {
            let intersection = matches!(&i.fields, Fields::Sop { dst: Some(Operand::Scalar { code: 126, dwords: 2 }), src, .. }
                if i.name() == "s_and_b64" && src.contains(&Operand::Scalar { code: 126, dwords: 2 }));
            let compare = matches!(&i.fields, Fields::Valu(_)) && i.name().starts_with("v_cmpx_");
            if !intersection && !compare {
                return Err("loop may resurrect missing EXEC lanes");
            }
        }
    }
    let counters: Vec<_> = addresses.iter().step_by(64).copied().collect();
    for (t, &a) in addresses.iter().enumerate() {
        if a != counters[t / 64]
            || a % 4 != 0
            || a.checked_add(4).is_none_or(|end| end > cs.lds_bytes())
        {
            return Err("counter is not one in-bounds dword per wave");
        }
    }
    let unique: std::collections::HashSet<_> = counters.iter().collect();
    if unique.len() != counters.len() {
        return Err("logical waves alias the same counter");
    }
    // All loop-carried writes become unknown. An invariant address/addend
    // must survive this operation; a convenient first-iteration value is not
    // sufficient. This also refuses indirect addresses depending on a loop value.
    for i in &loop_block.insts {
        for &(r, _) in &i.writes {
            state.forget(r);
        }
    }
    if state.addresses(add)? != addresses {
        return Err("counter address changes in the loop");
    }
    let Fields::Ds { data, .. } = &add.fields else {
        unreachable!()
    };
    if (0..state.threads).any(|t| state.operand(data[0], t) != Some(1)) {
        return Err("only invariant unit contributions proved");
    }
    let mut publication = None;
    let mut lane_reg = None;
    let mut barrier = false;
    for i in &p.blocks[2].insts {
        if i.name() == "s_barrier" {
            barrier = true;
        }
        if !barrier && ds(i) {
            let at = state.addresses(i)?;
            if publication.is_none() {
                if i.name() != "ds_read_b32" || at != addresses || !state.full() {
                    return Err("first observation is not a full-EXEC own-counter read");
                }
                lane_reg = (0..native.vgprs as usize).find(|&r| {
                    state.v[r]
                        .iter()
                        .enumerate()
                        .all(|(t, &v)| v == Some((t % 64) as u32))
                });
                publication = Some(i.machine.offset);
            } else if at.iter().any(|a| {
                counters
                    .iter()
                    .any(|c| *a < c + 4 && *c < a.saturating_add(4))
            }) && !(i.name() == "ds_read_b32" && at == addresses)
            {
                return Err(
                    "a peer or alias observes the private counter before the group barrier",
                );
            }
            if !matches!(i.name(), "ds_read_b32" | "ds_write_b32") {
                return Err("unproved LDS access width before barrier");
            }
        } else if !barrier
            && !ordinary(i)
            && !(publication.is_some()
                && matches!(
                    i.fields,
                    Fields::Smem { .. }
                        | Fields::Buffer { lds: false, .. }
                        | Fields::Flat { lds: false, .. }
                ))
        {
            return Err("untracked effect before publication barrier");
        }
        state.step(i);
    }
    let publication = publication.ok_or("missing own-counter publication")?;
    if counters.len() > 1 && !barrier {
        return Err("peer observations require an explicit group barrier");
    }
    let lane_reg = lane_reg.ok_or("no proven lane index at publication")? as u32;
    let used = |r| {
        p.blocks
            .iter()
            .flat_map(|b| &b.insts)
            .any(|i| i.read(Reg::S(r)).is_some() || i.write(Reg::S(r)).is_some())
    };
    let base = (0u8..=100)
        .step_by(2)
        .find(|&r| (r..r + 6).all(|r| !used(r)))
        .ok_or("no free scalar temporaries")?;
    let counter = u32::from(base);
    let tmp = counter + 1;
    let saved_exec = counter + 2;
    let saved_vcc = counter + 4;
    let sop1 = |op: u32, dst: u32, src: u32| 0xBE80_0000 | dst << 16 | op << 8 | src;
    let mut before = std::collections::HashMap::new();
    before.insert(loop_block.offset, vec![sop1(3, counter, 128)]);
    let mut publish = vec![
        sop1(4, saved_exec, 126),
        sop1(4, saved_vcc, 106),
        0x7C00_0000 | 0xD2 << 17 | lane_reg << 9 | 128,
        0x7E00_0200 | u32::from(native.vgprs) << 17 | counter,
    ];
    let observation = p.blocks[2]
        .insts
        .iter()
        .find(|i| i.machine.offset == publication)
        .ok_or("publication instruction missing")?;
    let Fields::Ds {
        addr: Some(Operand::Vector {
            reg: address_reg,
            dwords: 1,
        }),
        offset0,
        offset1,
        ..
    } = observation.fields
    else {
        return Err("counter address needs a vector dword");
    };
    publish.extend([
        0xD800_0000 | 0x0D << 18 | u32::from(offset0) | u32::from(offset1) << 8,
        u32::from(native.vgprs) << 8 | u32::from(address_reg),
        sop1(4, 106, saved_vcc),
        sop1(4, 126, saved_exec),
    ]);
    before.insert(publication, publish);
    let decoded = crate::gfx10::decode(&native.code).map_err(|_| "decode before rewrite")?;
    let replacement = vec![
        sop1(0x10, tmp, 126),
        0x8000_0000 | counter << 16 | tmp << 8 | counter,
    ];
    let mut mapping = std::collections::HashMap::new();
    let mut words = Vec::new();
    let mut relocations = Vec::new();
    for i in &decoded {
        // A branch to the loop skips the new, once-only initializer.
        if let Some(extra) = before.get(&i.offset) {
            words.extend(extra);
        }
        mapping.insert(i.offset, words.len());
        if i.offset == add.machine.offset {
            words.extend(&replacement);
        } else {
            if let Fields::Sopp { imm } = i.fields(true)
                && i.name()
                    .is_some_and(|n| n.starts_with("s_cbranch") || n == "s_branch")
            {
                relocations.push((
                    words.len(),
                    (i.offset as isize + 4 + isize::from(imm as i16) * 4) as usize,
                ));
            }
            words.extend(&i.words);
        }
    }
    for (at, target) in relocations {
        let target = *mapping.get(&target).ok_or("unresolved branch target")?;
        let delta = i16::try_from(target as isize - at as isize - 1)
            .map_err(|_| "relocated branch too far")?;
        words[at] = words[at] & 0xFFFF_0000 | u32::from(delta as u16);
    }
    let mut transformed = native.clone();
    transformed.code = words.into_iter().flat_map(u32::to_le_bytes).collect();
    transformed.vgprs += 1;
    let _ = init;
    Ok(Promotion {
        native: transformed,
        counter_addresses: counters,
        atomic_offset: add.machine.offset,
        publication_offset: publication,
        scalar_words: 6,
        vector_words: 1,
    })
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn null_is_zero_for_both_words_of_a_scalar_move() {
        // NULL is a special source, not the first word of a register pair.
        // An independent expected value must not come from State::operand.
        let words = [0xBEA4_047D, 0xBF81_0000]; // s_mov_b64 s[36:37], null
        let code: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let p = crate::ir::build(&code, true, 1).unwrap();
        let mut state = State::new(96);
        state.step(&p.blocks[0].insts[0]);
        assert!(state.s[36].iter().all(|v| *v == Some(0)));
        assert!(
            state.s[37].iter().all(|v| *v == Some(0)),
            "NULL's high word must not read EXEC_LO"
        );
    }
}
