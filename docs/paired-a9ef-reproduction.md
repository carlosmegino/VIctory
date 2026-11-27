# Exact paired control for #3180

The requested control is **a9ef792f6d516ee6200f4fedeb061a25153d48fd**, now
fetchable from <https://github.com/carlosmegino/VIctory> on
`codex/repro-paired-a9ef792`. This reference points at the original commit,
not a rebase or a later paired implementation. Its base is
`842b0cb76f8f5fa9289bd779783c51ec24d75302`.

The separate `codex/repro-paired-a9ef792-kit` branch adds this guide, test-only
adapters and diagnostic runners. Every original tracked file is unchanged,
except two appended `#[cfg(all(test, windows))]` module declarations in
`emit/mod.rs`. In particular the compiler, original test host, cache key and
runtime code remain byte-identical. Verify this with the supplied script.
The companion is not a replacement compiler revision.

## Provenance and attribution

The three commits on the main-based port are:

1. `6ecbbe833f2c5666d320ad4ec35a896b7c82f17a`: compute-only adaptation of
   Pilote Production/PiloteCode's GPL-3.0 implementation in
   [fxceless/victory-fork#854](https://github.com/fxceless/victory-fork/pull/854),
   source `26600c61e3dff95d188cbab7edfdf6b8b827fbca`. The original commit retains
   its Pilote Production co-author trailer. It also ports the masked-first-lane
   selection correction developed in `de64bd56fc177a49ca83db2528ca530b4cdcf99f`
   and adapted to the fork in `805a455d90e89cfa44572d19757c1454ee733136`.
2. `37966d85330db32904443efb99abfb645a96e6d6`: synthetic seven-UAV oracle and
   multidimensional IDs, adapting the earlier `1d717d5`/`805a455` tests and
   PiloteCode's LDS fixture.
3. `a9ef792f6d516ee6200f4fedeb061a25153d48fd`: runtime dispatch, guest UAV
   descriptors and tracked-memory readback, extending `b252f2d`'s PSO test.

The masked-first-lane preference is visible directly in `6ecbbe8`'s
`emit/mod.rs` change: on a Wave32-only target, a guarded, non-waterfall
`v_readfirstlane_b32` requests paired lowering before the ordinary form.
The compute port retains whole-Wave64 group and conservative barrier/operation
admission checks. It is not a port of every feature of the original fork.

The companion contract copies #3180's legally waited `guest()` and CPU
`oracle()` **unchanged** from `1625d1c686adc74e986d578b47aa6364b672b8d6`.
Only the translator API, module name, diagnostics option and metadata reporting
are adapted. Their SHA256 values are:

- guest: `e416f90aad5ce9e3b6fbfb090608b8d4ce49b9856d07dd3332aed06d0433e5f3`
- oracle: `17479ffaba08ee024e7c84d518b380485aa46e935fd54df01c03f3014a8f3427`

The lane-loop `program()` is taken from r4tedx's GPL-3.0-or-later
[#439 source](https://github.com/r4tedx/VIctory/blob/e2d3cfd50715954fb89ff462edb37a0c4395ecd0/crates/victory-shader/src/emit/lane_read_loop_tests.rs).
Its instruction construction and expected `t + lane` values are preserved.
The source history also credits ifBars for the deferred mask-phi work.
The public first-lane programs and native gfx1036 measurements belong to
[Lyrnc](https://gist.github.com/Lyrnc/00c6589f26930d5d07c077f8c4a30131).
Their files say LOCAL/not for commit, so they are fetched separately into an
external directory, pinned by raw revision and SHA256. They are not vendored.

## Differences from 140958f and b3060be

These are distinct source histories, not equivalent passing controls.
Their shared merge-base with a9ef792 is
`ffe310eb2098e52169f4c44db5ffdabf43a72622`.

| Revision | Role and material differences |
| --- | --- |
| `a9ef792` | Main-based compute-only port; `Options::wave32_only`, runtime capability selection and a separate cache profile; paired implementation in `emit/lower/paired.rs`; whole logical waves, restricted lane operations and explicit barrier convergence checks. Its exact historical fixtures are 64/128, not Dathinel's later partial cases. |
| `140958f228b31243588c36cd2edad09ada6702a2` | PiloteCode's merge of `805a455` and `b252f2d` onto `51700c6`; the fork's wider compiler/runtime and inline paired implementation in `lower.rs`, with global host-wave targeting. The preference uses `if let Ok(paired(...))`, so a refused partial group can fall back to an approximate form. In contrast, the main port propagates a forced paired error. |
| `b3060be56a0734ca7eb09b4479949d198656960b` | Descendant of 140958f, through the Dathinel fixture integration, refusal propagation, the partial-paired prototype and compiler commit `f762c6d4307f51f486453a96b3f3042eaf87dbb4`. Adds `partial_pairs.rs`, presence/EXEC/memory/barrier/LDS admission and runtime tests. Relative to 140958f it changes eight files (1,467 insertions, 39 deletions). The earlier 96/97/256 results belong to those revisions, not to a9ef792. |

The b3060be history is independently fetchable as
`codex/archive-paired-b3060be` in the same public repository. That history also
contains 140958f, 805a455, b252f2d and the original paired source lineage.
The later `741aa8e` review is another revision and is not used as this control.

Inspect the original commits and differences without changing either branch:

```sh
git fetch origin refs/heads/codex/repro-paired-a9ef792
test "$(git rev-parse FETCH_HEAD)" = a9ef792f6d516ee6200f4fedeb061a25153d48fd
git fetch origin refs/heads/codex/archive-paired-b3060be
git log --reverse --oneline 842b0cb76f8f5fa9289bd779783c51ec24d75302..a9ef792f6d516ee6200f4fedeb061a25153d48fd
git show 6ecbbe833f2c5666d320ad4ec35a896b7c82f17a -- crates/victory-shader/src/emit/mod.rs
git show 805a455d90e89cfa44572d19757c1454ee733136 -- crates/victory-shader/src/emit/mod.rs
git diff --stat a9ef792 140958f -- crates/victory-shader
git diff --stat 140958f b3060be -- crates/victory-shader
```

## Linux/Wine reproduction

Use an existing working Wine/vkd3d D3D12 SM6.6 prefix on a physical Wave32-only
GPU. Record Wine, vkd3d, driver and any diagnostic patches/vendor masking.
This kit does not certify a modified vkd3d stack or claim a local Linux run.
Windows-native runs below are a separate environment.

The helpers are Bash scripts using Python 3's standard library. Install the pinned Rust toolchain/target, LLVM clang/lld and
[cargo-xwin](https://github.com/rust-cross/cargo-xwin). cargo-xwin documents
Windows MSVC cross-compilation and `cargo xwin test` with Wine.
Supply a Windows x64 `dxil.dll` validator from your existing SDK/DXC setup;
missing validation fails rather than silently skipping it.

```sh
git clone --single-branch --branch codex/repro-paired-a9ef792-kit https://github.com/carlosmegino/VIctory.git victory-a9ef-repro
cd victory-a9ef-repro
# Pin the companion commit linked in the #3180 reply before recording results.
git rev-parse HEAD
git fetch origin 1625d1c686adc74e986d578b47aa6364b672b8d6
bash tools/verify-paired-repro.sh

rustup toolchain install 1.98.1 --profile minimal
rustup target add --toolchain 1.98.1 x86_64-pc-windows-msvc
cargo install --locked cargo-xwin
export CARGO_TARGET_DIR="$PWD/target/paired-repro"
cargo xwin test --target x86_64-pc-windows-msvc --release --locked -p victory-shader --lib --no-run

set -- target/paired-repro/x86_64-pc-windows-msvc/release/deps/victory_shader-*.exe
test "$#" -eq 1
EXE="$1"
DXIL=/absolute/path/to/windows-x64/dxil.dll
export VICTORY_DXIL="$(winepath -w "$DXIL")"
OUT="$PWD/../a9ef-repro-evidence"

# CPU guest wait/branch audit + exact 64/128 CPU/readback oracles.
bash tools/run-paired-repro.sh "$EXE" "$OUT/contract" --wine --suite contract

# Independent native-gfx1036 oracle and #439's three loop consumers.
bash tools/paired-repro-oracle.sh fetch "$OUT/consumers"
bash tools/run-paired-repro.sh "$EXE" "$OUT/consumers" --wine --suite consumers
```

Each test is a separate process with a 120-second process bound; the parent
also isolates the contract's validation child. A missing device, validator,
complete output, test selection or an unexpected SKIP is not accepted as
successful validation. The immutable backend itself has no new fence timeout;
the process bound does not cancel GPU work already submitted.

`--wine` explicitly sets `VICTORY_REPRO_NO_DEBUG=1`, omitting Windows Graphics
Tools/InfoQueue/GBV requirements while preserving mandatory DXIL validation,
physical-wave/group metadata checks, all lane values and canaries. This is
**not** a claim of Windows debug-layer or GBV coverage. The same explicit
no-debug contract path was tested on Windows, but Wine execution remains an
independent validation request.

For Windows-native validation, use ordinary `cargo test --release --locked
-p victory-shader --lib --no-run`, set VICTORY_DXIL to the SDK validator and
invoke the runner through Git Bash **without** `--wine` (set `PYTHON` to your Python 3 executable if necessary). Contract tests enable D3D12 debug/GBV
before first device creation. Consumer diagnostics enable the debug layer,
but do not claim complete InfoQueue/GBV certification. New output directories
are required; existing evidence logs are not overwritten.

The consumer suite is deliberately red on this historical compiler. Its
exit status must not be discarded or converted to a passing backend verdict.
`firstlane-actual.tsv` contains all 10,240 words, in register-major order
(`64 * register_index + lane`); the separate comparator maps them to Lyrnc's
lane-major oracle. `run.json` records source status and executable SHA256.

### Exact historical tests, without the companion

To run the original Windows/Graphics Tools fixtures at precisely the original
commit, use a second checkout and the two tests in fresh processes:

```sh
git clone --single-branch --branch codex/repro-paired-a9ef792 https://github.com/carlosmegino/VIctory.git victory-a9ef-exact
cd victory-a9ef-exact
test "$(git rev-parse HEAD)" = a9ef792f6d516ee6200f4fedeb061a25153d48fd
cargo test -p victory-shader --release --locked --lib --no-run
cargo test -p victory-shader --release --locked --lib -- --exact emit::wave64_group_tests::wave64_auto_group_64 --ignored --nocapture --test-threads=1
cargo test -p victory-shader --release --locked --lib -- --exact emit::wave64_group_tests::wave64_auto_group_128 --ignored --nocapture --test-threads=1
```

Those original fixtures retain their original instruction/wait sequence and
debug-layer requirement; the companion additionally supplies the waited
1625d1c fixture. No compiler equivalence with a later revision is assumed.

## Recorded results and limits — 2026-11-27

Local machine: Windows, RTX 5070 Ti (vendor 0x10de, device 0x2c05, lanes 32/32),
driver 621.01, Rust 1.98.1, SDK 10.0.26100.0 validator. Source worktrees were
isolated from ongoing #2633/partial-paired work. Only synthetic cases ran.

| Check | Result |
| --- | --- |
| Exact a9ef792 original 64/128 tests | Both pass; 448 and 896 lane values, 56 canaries each, no D3D12/GBV warnings/errors. |
| Waited #3180 companion 64/128 | Both pass; physical groups 32/64 with required Wave32; same value/canary counts and no native D3D12/GBV warnings/errors. CPU wait/branch audit also passes. |
| Explicit no-debug companion 128 | Passes on Windows; not a Wine or GBV result. |
| #439 Wave32/lane20 ordinary latch | 12/32 wrong: lane0 returns 0 rather than 20; lanes21..31 keep t rather than t+20. |
| #439 Wave64/lane40 ordinary latch | 0/64 wrong; paired physical group 32, required Wave32. |
| #439 Wave64 joined EXEC latch | Refused during emission: untracked lane-mask join at 0x28. No PSO/readback claimed. |
| Lyrnc firstlane_branch | All ten cases validate/create PSOs/dispatch; 508/10,240 words differ (51 in v10, 457 in v11). The none mask agrees. This matches the reported mismatch total, not a successful general scalar-merge control. |

The first-lane trace reports `has EXEC guard=false, waterfall=false` at the
read site in these ten cases. They take 64 physical threads with unrestricted
wave metadata, bypassing this port's guarded-first-lane paired preference.
That is a routing observation, not proof that changing one predicate fixes
scalar reconvergence. Native Wave32 never enters the Wave64 paired path, so
its 12 wrong lanes remain a separate ordinary-flow gate.

The #2633 706c006 reports (14/160 loop mismatches and the retention-only
probe that did not fix them) are external Linux/Wine evidence by pickles976,
with TaynanGT's comparison and Lyrnc's native oracle. They are not measurements
of that branch on this machine. `wide_keep`'s per-lane zero/retention policy
and scalar `wide_phi`/`wide_pick` edge selection both require owner review;
do not remove retention globally or mix scalar-wave state with VGPR/EXEC
state or independent logical waves that have finished.

Chippy01's newer #1633 claim
[7220136661](https://github.com/Marvisak/VIctory/issues/1633#issuecomment-7220136661)
owns further #2633 whole-wave region/section work. This package contributes
matched evidence and reproducibility, not a competing scalar-flow patch.

### AMD gate and checks

[KilolimaVI's independent result](https://github.com/Marvisak/VIctory/pull/3180#issuecomment-7217050790)
on #3180 head 1625d1c, Windows 11/Radeon 780M RDNA3, Graphics Tools and Rust
1.98.1 confirms both tests report SKIP with WaveOps=true and lanes (32,64),
then pass 2/2 instead of panicking. This validates exclusion only, not paired
backend correctness on AMD. The exact control still has its historical gate;
the revised gate is in the copied #3180 companion and in #3180 itself.

Policy passes on the exact historical a9ef792 (194 files) and b3060be (195).
The companion's new Rust files are rustfmt-formatted. Clippy all shader
targets/features with `-D warnings` reports the **pre-existing** misplaced
SAFETY comment at original `wave64_group_tests.rs:203`; it is preserved to
keep the exact source intact. No check is disabled and no green Clippy claim
is made. Existing whole-file/main formatting differences are likewise outside
this reproduction package. Full workspace suites and title tests were not run.
