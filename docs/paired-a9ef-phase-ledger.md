# Exact a9ef792: phase-separated evidence

This is a documentation/data-only supplement to the immutable
[`2b5c107` reproduction kit](https://github.com/carlosmegino/VIctory/blob/2b5c107606a2d9632973e21b4a26022333730cef/docs/paired-a9ef-reproduction.md).
It reuses existing synthetic readbacks; no GPU tests or compiler changes were
made for this supplement. The exact backend is still
`a9ef792f6d516ee6200f4fedeb061a25153d48fd`, not b3060be or 741aa8e.

## Access, base and masked-first-lane correction

- Repository: https://github.com/carlosmegino/VIctory
- Exact branch: `codex/repro-paired-a9ef792` ->
  `a9ef792f6d516ee6200f4fedeb061a25153d48fd`.
- Base: `842b0cb76f8f5fa9289bd779783c51ec24d75302`.
- Compute-port/masked-first-lane commit:
  [`6ecbbe833f2c5666d320ad4ec35a896b7c82f17a`](https://github.com/carlosmegino/VIctory/commit/6ecbbe833f2c5666d320ad4ec35a896b7c82f17a).
  The `emit/mod.rs` diff contains the early paired selection for guarded,
  non-waterfall first-lane reads; provenance is de64bd5/805a455 and PiloteCode's
  #854 source 26600c6. Original authorship/co-author trailers are preserved.
- Compiler/host-preserving kit: `2b5c107606a2d9632973e21b4a26022333730cef`.

Independent Git verification used a newly initialized bare repository with
system/global configuration disabled, no credential helper, no alternate
object directories and HTTPS fetch. The original branch was fetched directly
before the kit. Both the exact head and its base were checked. This is an
independent Git object/configuration environment on the same Windows machine;
it is not a claim of a second machine or a Linux GPU run.

There is also separate independent-machine evidence:
[pickles976's exact-head Linux/Wine report](https://github.com/Marvisak/VIctory/pull/3180#issuecomment-7220146530)
builds the a9ef792 Windows MSVC target under Rust 1.98.1 and exports the
synthetic containers/reference files (1/1 pass). Original GPU tests stop at
missing Windows Graphics Tools/InfoQueue interfaces before any dispatch.
That confirms independent access/build, not successful paired readback under
Wine. The kit's explicit Wine/no-debug mode addresses the shader-host harness
requirement; it does not retrofit or certify the original GPU-runtime test.

An equivalent clean fetch on Linux is:

```sh
mkdir paired-fetch-independent
cd paired-fetch-independent
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_TERMINAL_PROMPT=0
unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
git init --bare exact.git
git -c credential.helper= -C exact.git fetch --depth=4 https://github.com/carlosmegino/VIctory.git refs/heads/codex/repro-paired-a9ef792
test "$(git -C exact.git rev-parse FETCH_HEAD)" = a9ef792f6d516ee6200f4fedeb061a25153d48fd
test "$(git -C exact.git rev-parse FETCH_HEAD~3)" = 842b0cb76f8f5fa9289bd779783c51ec24d75302
git -C exact.git show 6ecbbe833f2c5666d320ad4ec35a896b7c82f17a -- crates/victory-shader/src/emit/mod.rs
```

## Inside the branch versus after reconvergence

[The phase ledger](paired-a9ef-phase-outputs.json) preserves the actual and
native-oracle expected value for every lane of both snapshots, not just a
combined mismatch total. It also records the raw TSV SHA256, executable SHA256,
source revision, layout and pinned external oracle. Inputs and the original
reference belong to Lyrnc; they remain separately fetched, not vendored.

- **v10** captures the scalar while the conditional branch executes. Lanes
  outside the branch retain the initialized v10. The captured VGPR is stored
  at the end of the shader; it is an inside-branch snapshot, not a second
  mid-dispatch capture.
- **v11** captures the scalar after EXEC is restored. All 64 lanes must be
  checked, including lanes that never entered the branch.
- Raw memory layout is register-major: `64 * (VGPR - 10) + lane`. The native
  gfx1036 oracle is lane-major and is transposed explicitly by the comparator.

| Mask | Inside branch v10 | After reconvergence v11 |
| --- | ---: | ---: |
| all | 32/64 wrong | 32/64 wrong |
| lo32 | 0/64 wrong | 32/64 wrong |
| hi32 | 0/64 wrong | 32/64 wrong |
| alt | 16/64 wrong | 48/64 wrong |
| lane0 | 0/64 wrong | 63/64 wrong |
| lane40 | 0/64 wrong | 63/64 wrong |
| lane63 | 0/64 wrong | 63/64 wrong |
| l31_32 | 1/64 wrong | 63/64 wrong |
| sparse | 2/64 wrong | 61/64 wrong |
| none | 0/64 wrong | 0/64 wrong |
| **Total** | **51/640 wrong** | **457/640 wrong** |

All other 8,960 words agree, so the complete comparison remains **508/10,240**.
For `lo32`, lane 32 has v10=0 as initialized, which agrees, but v11=0x5555
instead of 5 after reconvergence. For `all`, lane 32 has v10=101 and v11=101
instead of 5. Those are separate observations, not one generic output failure.

These ten cases bypass the port's guarded-first-lane preference: the diagnostic
reports no instruction EXEC guard at the read and no waterfall flag. They run
the ordinary 64-physical-thread form. The presence of the masked-first-lane
correction in the archive therefore does not make these cases exact.

## Native Wave32 consumer, explicitly separate

The unchanged #439 constructor is `program(32, 20, false)`:
**native guest Wave32**, one 32-thread group, reading lane 20 inside a loop
and storing `t + s9` after restoring EXEC. It does not use Wave64 paired
lowering. Expected output is `[20, 21, ..., 51]`; the archived compiler has
12 wrong lanes: lane 0 returns 0 instead of 20; lanes 21..31 return t instead
of t+20. Lane 0 never entered the loop but still needs its wave's scalar update.

The other two consumers are distinct:

- `program(64, 40, false)`: paired physical group 32, required Wave32;
  **0/64 wrong**, expected `[40, ..., 103]`.
- `program(64, 40, true)`: emission refusal at 0x28, untracked lane-mask join;
  **no PSO, no readback**. Do not call the three consumers 0/160 or use a
  refused program as correctness evidence.

The 64/128 seven-UAV contracts still pass (448/896 values plus 56 canaries
each). Those particular passes do not override the red consumer/oracle gates.

## Reproduction and expected status

Use the complete Linux/Wine prerequisites and commands in the
[pinned kit guide](https://github.com/carlosmegino/VIctory/blob/2b5c107606a2d9632973e21b4a26022333730cef/docs/paired-a9ef-reproduction.md).
The essential source selection is:

```sh
git clone --single-branch --branch codex/repro-paired-a9ef792-kit https://github.com/carlosmegino/VIctory.git victory-a9ef-repro
cd victory-a9ef-repro
git switch --detach 2b5c107606a2d9632973e21b4a26022333730cef
git fetch origin 1625d1c686adc74e986d578b47aa6364b672b8d6
bash tools/verify-paired-repro.sh
export CARGO_TARGET_DIR="$PWD/target/paired-repro"
cargo xwin test --target x86_64-pc-windows-msvc --release --locked -p victory-shader --lib --no-run
set -- target/paired-repro/x86_64-pc-windows-msvc/release/deps/victory_shader-*.exe
test "$#" -eq 1
EXE="$1"
DXIL=/absolute/path/to/windows-x64/dxil.dll
export VICTORY_DXIL="$(winepath -w "$DXIL")"
OUT="$PWD/../a9ef-phase-repro"
bash tools/run-paired-repro.sh "$EXE" "$OUT/contract" --wine --suite contract
bash tools/paired-repro-oracle.sh fetch "$OUT/consumers"
bash tools/run-paired-repro.sh "$EXE" "$OUT/consumers" --wine --suite consumers
```

The Windows-native evidence above predicts a green contract suite, a red
consumer suite (exit 1), a lane-loop test exit 101, ten completed first-lane
dispatches, and a red oracle comparison (exit 1). Linux/Wine must independently
verify these observations; they are not promised cross-driver bit patterns.
Do not suppress failure statuses, skip the native Wave32 leg or replace the
expected scalar value in never-entered lanes. Wine mode deliberately omits
Windows debug/GBV interfaces, but still requires DXIL validation and complete
outputs. No Wine/GBV certification is inferred.

## Ownership, newer evidence and reserved water work

This is follow-up evidence under the existing NEW-354 claim. Scalar-flow
implementation on #2633 stays with Chippy01 and the upstream owners, with
TaynanGT and pickles976's review/diagnostic contributions. **TaynanGT's #208
matched water comparison remains reserved. No water/title shader, capture,
benchmark, branch or test was changed or run here.**

The edited [pickles976 report](https://github.com/Marvisak/VIctory/pull/2633#issuecomment-7217164469)
now links an owned divergent-scalar-join fix:
[mohammadallami75-ops's r4tedx/VIctory#14](https://github.com/r4tedx/VIctory/pull/14),
head `d21eb165c333de3f10d95558616b84d398df2d73`, base
`c7346008de48f6c71e22cdff0d5f7b644fe798fb`. The
[external physical-GPU A/B](https://github.com/r4tedx/VIctory/pull/14#issuecomment-7219801493)
reports 508 -> 0 first-lane mismatches and 0/160 loop mismatches on both its
base and head. That is its own #439 lineage, not a rerun of a9ef792. The
#2633 706c006 14/160 lane-loop result remains a separate owner gate.
No competing fix, cherry-pick or fresh validation of #14 was performed here.

The later partial-paired b3060be and review 741aa8e are likewise separate
source identities. None of their results appears in this phase ledger.
