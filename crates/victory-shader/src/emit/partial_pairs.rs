//! Admission for a partial final Wave64 on the paired backend.

use std::collections::HashMap;

use crate::ShaderError;
use crate::gfx10::{Fields, NULL, Operand};
use crate::ir::{EXEC, Exit, Inst, Program, Reg, Value};
use crate::native::{ComputeIo, NativeProgram, RootType};
use crate::resource::{Resources, Source};
use crate::structure::Structured;

pub(super) const LDS_BYTES: u32 = 32 * 1024;

pub(super) fn group_size(cs: ComputeIo) -> Result<u32, ShaderError> {
    let threads = cs.threads.into_iter().try_fold(1u32, u32::checked_mul);
    threads
        .filter(|&n| (1..=1024).contains(&n) && cs.threads[2] <= 64)
        .ok_or_else(|| unsupported(0, "invalid paired compute group dimensions"))
}

fn unsupported(offset: usize, what: &'static str) -> ShaderError {
    ShaderError::Unsupported { offset, what }
}

fn immediate(operand: Operand) -> Option<u64> {
    match operand {
        Operand::Const(c @ 128..=192) => Some(u64::from(c - 128)),
        Operand::Const(c @ 193..=208) => Some((192 - i64::from(c)) as u64),
        Operand::Literal(x) => Some(u64::from(x)),
        Operand::Scalar { code: NULL, .. } => Some(0),
        _ => None,
    }
}

fn mask_word(reg: Reg, base: u8) -> Option<u8> {
    if reg == Reg::S(base) {
        Some(0)
    } else if base.checked_add(1).is_some_and(|high| reg == Reg::S(high)) {
        Some(1)
    } else {
        None
    }
}

fn possible_bits(inst: &Inst, reg: Reg, bits: &HashMap<Value, u32>, present: u64) -> u32 {
    let name = inst.name();
    if name.starts_with("v_cmp_") || name.starts_with("v_cmpx_") {
        let Fields::Valu(v) = &inst.fields else {
            return u32::MAX;
        };
        let base = if name.starts_with("v_cmpx_") {
            EXEC
        } else if let Some(Operand::Scalar { code, .. }) = v.dst {
            code
        } else {
            return u32::MAX;
        };
        return mask_word(reg, base).map_or(u32::MAX, |word| (present >> (word * 32)) as u32);
    }
    let Fields::Sop {
        dst: Some(Operand::Scalar { code, dwords: 2 }),
        src,
        ..
    } = &inst.fields
    else {
        return u32::MAX;
    };
    let read = |reg| {
        inst.read(reg)
            .and_then(|v| bits.get(&v).copied())
            .unwrap_or(u32::MAX)
    };
    let word = mask_word(
        reg,
        if name == "s_and_saveexec_b64" && mask_word(reg, EXEC).is_some() {
            EXEC
        } else {
            *code
        },
    );
    let Some(word) = word else { return u32::MAX };
    let arg = |n: usize| match src.get(n).copied() {
        Some(Operand::Scalar { code, .. }) if code != NULL => code
            .checked_add(word)
            .map_or(u32::MAX, |reg| read(Reg::S(reg))),
        Some(op) => immediate(op).map_or(u32::MAX, |v| (v >> (word * 32)) as u32),
        None => u32::MAX,
    };
    match name {
        "s_mov_b64" => arg(0),
        "s_and_b64" => arg(0) & arg(1),
        "s_andn2_b64" => arg(0),
        "s_or_b64" | "s_xor_b64" | "s_cselect_b64" => arg(0) | arg(1),
        "s_and_saveexec_b64" if mask_word(reg, EXEC).is_some() => {
            read(Reg::S(EXEC + word)) & arg(0)
        }
        "s_and_saveexec_b64" => read(Reg::S(EXEC + word)),
        _ => u32::MAX,
    }
}

/// May-be-set bits in the final wave. Start loop phis empty, then add the
/// union of incoming bits to a fixed point. Unknown scalar values are all ones.
fn mask_bounds(p: &Program, present: u64) -> HashMap<Value, u32> {
    let order = crate::ir::reverse_postorder(&p.blocks);
    let mut bits = HashMap::new();
    for &(reg, value) in &p.inputs {
        bits.insert(
            value,
            mask_word(reg, EXEC).map_or(u32::MAX, |w| (present >> (w * 32)) as u32),
        );
    }
    for &b in &order {
        let block = &p.blocks[b];
        bits.extend(block.phis.iter().map(|phi| (phi.value, 0)));
        bits.extend(
            block
                .insts
                .iter()
                .flat_map(|i| i.writes.iter().map(|&(_, v)| (v, 0))),
        );
    }
    loop {
        let mut changed = false;
        for &b in &order {
            let block = &p.blocks[b];
            for phi in &block.phis {
                let next = phi
                    .args
                    .iter()
                    .fold(0, |m, v| m | bits.get(v).copied().unwrap_or(u32::MAX));
                changed |= merge_bits(&mut bits, phi.value, next);
            }
            for inst in &block.insts {
                for &(reg, value) in &inst.writes {
                    let next = possible_bits(inst, reg, &bits, present);
                    changed |= merge_bits(&mut bits, value, next);
                }
            }
        }
        if !changed {
            return bits;
        }
    }
}

fn merge_bits(bits: &mut HashMap<Value, u32>, value: Value, next: u32) -> bool {
    let old = bits.get_mut(&value).expect("definition indexed above");
    let changed = next & !*old != 0;
    *old |= next;
    changed
}

/// Require each barrier to occur exactly once per physical wave on all paths.
/// Wave-divergent loops may precede it if the lowerer's loop cap exits through
/// the CFG. An unconditional backedge can instead abort the entire shader.
fn check_barriers(p: &Program, threads: u32) -> Result<(), ShaderError> {
    let order = crate::ir::reverse_postorder(&p.blocks);
    let barriers: Vec<_> = order
        .iter()
        .copied()
        .filter(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "s_barrier"))
        .collect();
    if (threads > 64 || !barriers.is_empty()) && super::group_sync() != 2 {
        return Err(unsupported(
            0,
            "partial paired waves require group synchronization",
        ));
    }
    if barriers.is_empty() {
        return Ok(());
    }
    let mut rank = vec![usize::MAX; p.blocks.len()];
    for (n, &b) in order.iter().enumerate() {
        rank[b] = n;
    }
    let visit = |mut stack: Vec<usize>, omit: Option<usize>, fail: &dyn Fn(usize) -> bool| {
        let mut seen = vec![false; p.blocks.len()];
        while let Some(b) = stack.pop() {
            if Some(b) == omit || std::mem::replace(&mut seen[b], true) {
                continue;
            }
            if fail(b) {
                return true;
            }
            stack.extend(p.blocks[b].exit.successors());
        }
        false
    };
    for barrier in barriers {
        let can_skip = visit(vec![0], Some(barrier), &|b| match p.blocks[b].exit {
            Exit::End | Exit::Indirect => true,
            Exit::Jump(t) => rank[t] <= rank[b],
            Exit::Branch { .. } => false,
        });
        let can_repeat = visit(p.blocks[barrier].exit.successors(), None, &|b| b == barrier);
        if can_skip || can_repeat {
            return Err(unsupported(
                p.blocks[barrier].offset,
                "partial paired barrier has no proven group rendezvous",
            ));
        }
    }
    Ok(())
}

pub(super) fn check(
    native: &NativeProgram,
    p: &Program,
    st: &Structured,
    res: &Resources,
    threads: u32,
) -> Result<(), ShaderError> {
    check_barriers(p, threads)?;
    let tail = threads % 64;
    let present = (1u64 << tail) - 1;
    let bits = mask_bounds(p, present);
    let bounded = |value, word: u32| {
        let allowed = (present >> (word * 32)) as u32;
        bits.get(&value).is_some_and(|&mask| mask & !allowed == 0)
    };
    // A raw lane mask may be read as data or used by SCC. The paired ballot
    // ignores absent host slots, so it can only represent bounded masks.
    if st.masks.keys().any(|&low| !bounded(low, 0))
        || st.highs.keys().any(|&high| !bounded(high, 1))
    {
        return Err(unsupported(
            0,
            "partial paired lane mask may contain bits outside the guest group",
        ));
    }
    if !res.waterfall.reads.is_empty()
        || res.descriptors.iter().any(|d| {
            let Source::Loaded { load, .. } = d.source else {
                return true;
            };
            !res.scalar.iter().any(|&(site, addr)| {
                site == load
                    && addr.dynamic.is_none()
                    && native
                        .layout
                        .get(addr.param)
                        .is_some_and(|p| p.root == RootType::Table)
            })
        })
    {
        return Err(unsupported(
            0,
            "partial paired lanes require statically traced table descriptors",
        ));
    }
    for block in &p.blocks {
        for inst in &block.insts {
            let offset = inst.machine.offset;
            let name = inst.name();
            if inst
                .write(Reg::S(EXEC))
                .or(inst.write(Reg::S(EXEC + 1)))
                .is_some()
            {
                let pair = inst.write(Reg::S(EXEC)).zip(inst.write(Reg::S(EXEC + 1)));
                if !pair.is_some_and(|(lo, hi)| {
                    bounded(lo, 0) && bounded(hi, 1) && st.mask_bits(lo, Some(hi)).is_some()
                }) {
                    return Err(unsupported(offset, "unproven partial paired EXEC write"));
                }
            }
            let supported = match &inst.fields {
                Fields::Sop { .. } => matches!(name,
                    "s_mov_b32" | "s_mov_b64" | "s_and_b64" | "s_andn2_b64" | "s_or_b64"
                    | "s_xor_b64" | "s_cselect_b64" | "s_and_saveexec_b64" | "s_bcnt1_i32_b64"
                    | "s_add_u32" | "s_sub_u32" | "s_cmp_eq_u32" | "s_cmp_lg_u32"),
                Fields::Sopp { .. } => matches!(name, "s_barrier" | "s_branch" | "s_endpgm")
                    || name.starts_with("s_cbranch_"),
                Fields::Smem { .. } => name == "s_load_dwordx4" && res.descriptors.iter().any(|d| {
                    matches!(d.source, Source::Loaded { load, .. } if p.blocks[load.0].insts[load.1].machine.offset == offset)
                }),
                Fields::Valu(v) => v.dpp.is_none() && v.sdwa.is_none() && match name {
                    "v_readlane_b32" | "v_writelane_b32" => v.src.get(1).copied()
                        .and_then(immediate).is_some_and(|lane| (lane & 63) < u64::from(tail)),
                    "v_mov_b32" | "v_readfirstlane_b32" | "v_add_nc_u32" | "v_mad_u32_u24"
                    | "v_sub_nc_u32" | "v_or_b32" | "v_and_b32" | "v_xor_b32" | "v_cndmask_b32"
                    | "v_lshrrev_b32" | "v_lshlrev_b32" | "v_mul_lo_u32" => true,
                    _ => name.starts_with("v_cmp_") || name.starts_with("v_cmpx_"),
                },
                Fields::Ds { gds: false, .. } => matches!(name,
                    "ds_read_b32" | "ds_write_b32" | "ds_add_u32" | "ds_add_rtn_u32" | "ds_bpermute_b32"),
                Fields::Buffer { idxen: false, format: None, tfe: false, lds: false, .. } => {
                    matches!(name, "buffer_store_dword" | "buffer_load_dword" | "buffer_atomic_add")
                }
                _ => false,
            };
            if !supported {
                return Err(unsupported(
                    offset,
                    "instruction not certified for partial paired waves",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn paired_dimensions_check_valid_limits_and_overflow() {
        let group = |threads| super::group_size(super::ComputeIo { rsrc2: 0, threads });
        assert_eq!(group([3, 11, 3]).unwrap(), 99);
        assert_eq!(group([4, 4, 64]).unwrap(), 1024);
        for dims in [[0, 1, 1], [1025, 1, 1], [1, 1, 65], [u32::MAX, 2, 1]] {
            assert!(group(dims).is_err(), "invalid dimensions {dims:?}");
        }
    }
}
