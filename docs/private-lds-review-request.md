# Independent review requested: wave-private LDS promotion

**Archived at the user's request.** Implementation/evidence source
03be2d04130d62ea91997933e59d8ca6bfafe1ff is preserved. Do not generalize,
enable or benchmark this optimization further without independent review
or a demonstrated real application. The review request remains open; no
such review or real accepted program is being claimed.

**New result:** [the official #2633 comparison](private-lds-halves-result.md)
shows that original96 already passes upstream, while the promoted form has
192 incorrect counter values and more emitted barriers. Do not enable this
pass on halves. A synthetic repro and test-only installer are included.

This branch publishes an explicit compiler prototype for review, not an
enabled optimization or an integration proposal for an entire paired backend.
The analysis/rewrite is now in crates/victory-shader/src/private_lds.rs,
separate from emit and paired. Its only dependencies are native GFX10/IR
definitions. It remains compiled for tests only. No production lowerer or
previous PR is changed.

## What needs review

Please independently audit the privacy/alias proof, the unit no-return atomic
restriction, EXEC bounds, scalar effects, temporary allocation and relocation.
In particular: can any accepted program observe a changed atomic or memory
effect before publication? Can a logical wave affect another's register or
counter state? Does an accepted address escape the proven region? The current
proof deliberately rejects unknown answers, returned atomics, GDS and more
general regions/addends rather than weakening barriers or dropping operations.

The native guest rewrite is backend independent. Its execution is not:
another lowerer must preserve wave scalar semantics, count the logical EXEC,
and supply the required memory ordering. Halves may need cross-half trades
for the population count, so deleting guest LDS atomics alone does not prove
a synchronization or performance improvement. Compatibility with #2633
1b1ef2161a2f1cbcb46d7f72a7b24baf1303d419 is being checked separately.

## Existing evidence

At 42daca9 on the stable 741aa8e paired reference, the intact original
Dathinel 561503d 96-thread input through the explicit pass and an indirect
address/different-EXEC variant each passed 672 values and 56 guards, DXIL,
PSO and actual native Windows dispatch on RTX 5070 Ti, with D3D12/GBV.
Independent CPU oracles check all seven UAVs, including peer counters,
per-lane accumulation, firstlane and opposite-half exchange. Counters were
160/112 and 224/144. These are synthetic fixtures, not a title/BVH execution.

For original96, logical guest LDS accesses count 658 -> 388: 272 unit atomic
adds become two publication stores. Initial stores, the explicit barrier,
other LDS accesses and exports remain. Emitted paired DXIL still has two
backend allocator atomics and seven barrier calls. No timing, physical
register, occupancy or hardware-traffic improvement has been measured.
The old eighteen reduction refusals remain unvalidated execution.

Two follow-up proof weaknesses were reproduced before correction: NULL's
high dword in a 64-bit scalar move incorrectly read EXEC_LO, and inconsistent
VGPR metadata allowed a newly allocated vector temporary to name a register
already used by the guest. Both now have negative/independent expected-value
regressions. Four focused CPU tests pass after the fixes and the neutral
module move. Existing valid fixture proofs remain accepted. This is local
review evidence; external technical review is still requested.

## Reproduction

With Rust 1.98.1, the CPU review/analysis tests on native Windows:

```powershell
cargo test -p victory-shader --release --locked private_lds -- --nocapture
```

The opt-in seven-UAV readback test (native exclusively Wave32 adapter,
Windows Graphics Tools and SDK dxil.dll required):

```powershell
$env:VICTORY_DXIL = 'C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/dxil.dll'
cargo test -p victory-shader --release --locked paired_analyzed_original_96_matches_all_outputs -- --ignored --nocapture --test-threads=1
```

Use a fresh dedicated target directory for this checkout; do not mix artifacts
with an unrelated checkout. The raw original remains refused without the
explicit pass. Additional 64/97/128/256 counter proofs are not additional GPU
certifications. Broad all-target Clippy inherits seven failures in unchanged
baseline test files; the new proof checks do not suppress or edit those tests.

Attribution: Dathinel's synthetic guest/oracle (561503d), PiloteCode's paired
backend and host harness (#854), and the original GPL attribution are retained.
Lyrnc/ColorMeBoom's reduction audit is a separate preceding test contribution;
this does not copy wide_and or duplicate #2633 scalar/firstlane work, #269 or
#320. AI-assisted with OpenAI Codex. No title inputs or derived material are
published in this branch.
