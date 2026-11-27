# Independent Blackwell result against official #2633

Control b32cef72554981f89fad00fecc8358699518da8a; candidate
1b1ef2161a2f1cbcb46d7f72a7b24baf1303d419 (including 27029af).
Native Windows, RTX 5070 Ti (vendor 0x10de/device 0x2c05, physical waves
32/32), driver 621.01, Rust 1.98.1, SDK 10.0.26100.0 dxil.dll, D3D12 debug
and GPU-based validation. No Wine or WARP. These are new local results,
separate from Pickles976's RTX 4060 Laptop/Wine validation.

## Focused semantic checks

The archived ten original Lyrnc firstlane_branch inputs and native gfx1036
oracle were reused unchanged. Oracle SHA256:
eee0f705651c6083e3fbd8a0e3645ff55b66ed9222a68556b12a497477588715.

| Check | b32cef7 | 1b1ef216 |
| --- | ---: | ---: |
| firstlane v10, inside branch | 51 wrong | 0 wrong |
| firstlane v11, after reconvergence | 457 wrong | 0 wrong |
| other firstlane output words | 0 wrong | 0 wrong |
| original #439 Wave64 ordinary consumer | 1/64 wrong | 0/64 wrong |
| original #439 Wave64 joined-latch consumer | 1/64 wrong | 0/64 wrong |
| original #439 native Wave32 consumer | 12/32 wrong | 12/32 wrong |

Each firstlane arm compares all 10,240 words, with eight extra UAV guard
words per dispatch. Translation, DXIL validation, PSO and completed execution
all occurred. The consumer test retains the original zero-error assertion,
so its overall exit is nonzero because the native Wave32 negative still
fails; this is not reported as an all-passed test. The author's three new
regressions (wide-branch reduction, branch firstlane and loop scalars) also
pass with strict adapter setup and their original guest bodies/oracles.

The SDK emits a known host warning about COPY_DEST initial state being
ignored for buffers. That exact message was logged separately; other warning/
error messages remained failures. Do not describe this as zero raw warnings.
No water-path comparison, full 151-case run or BVH PSO census was repeated.
The remaining 20 cases and f32 summation order are outside these checks.

## Retained fallback is not semantic certification

A local routing audit found 89 firstlane-only routing attempts: 87 retained
the new form and two fell back. Both exact fallback programs translate,
validate and create PSO in both revisions; their unsigned DXIL is byte-for-byte
identical to b32cef7. Neither was dispatched, and no semantic oracle for
their original bindings was used. Their correctness remains unverified.

A separate synthetic negative reserves the entire LDS allocation while
keeping the author's 128-thread firstlane guest unchanged. It takes the
firstlane unroute fallback, validates/executes, and has 64/128 wrong lanes
(128 wrong values over two captured registers). This demonstrates that
retaining an older lowering can retain incorrect answers; it does not prove
that the two real fallback programs have the same runtime error.

## Promotion is not currently valid for halves

The backend-neutral pass was applied explicitly to the unchanged Dathinel
guest, with seven UAVs and all CPU values/56 guards checked per run:

| Threads | Form on 1b1ef216 | Wrong values | Static emitted barriers |
| --- | --- | ---: | ---: |
| 64 | original | 0/448 | 9 |
| 64 | promoted | 128/448 | 15 |
| 96 | original | 0/672 | 5 |
| 96 | promoted | 192/672 | 11 |

All four forms translate, validate, create PSO and complete execution. The
promoted96 errors are exactly the own/peer counter UAVs (96 each); the other
five UAVs and every guard match. Counters are 256/160, expected 160/112.
That is consistent with counting all present lanes on every loop visit;
this is a diagnostic hypothesis, not a claimed root-cause correction.

This differs from the paired reference, where the explicit promotion matches
all 672 values and 56 guards. The original96 admission/semantic goal is now
covered upstream without promotion. No partial-group coverage or barrier
reduction is gained here; enabling this pass on halves would be incorrect.
The analysis is separate from paired, but the new backend's execution of the
rewritten instruction sequence is not certified by the proof alone.

The real-input static privacy audit examined 1,166 archived Wave64 compute
programs: 59 contain ds_add_u32, and none meets this prototype's proof.
32 refuse layout/register limits; 27 refuse the current three-block shape.
Within those inputs, ten are BVH Wave64 programs, one has ds_add, none is
proved. This is not evidence that no broader optimization exists, and not
execution of any BVH program. No performance improvement is claimed.

## Reproduce the new backend comparison

Use an isolated checkout of exact 1b1ef216; from this review branch:

```powershell
./tools/prepare-halves-private-counter.ps1 -Checkout <path-to-1b1ef216>
cd <path-to-1b1ef216>
$env:CARGO_TARGET_DIR = '<fresh-dedicated-target-directory>'
$env:VICTORY_DXIL = 'C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/dxil.dll'
cargo test -p victory-shader --release --locked private_lds_halves_compatibility -- --ignored --nocapture --test-threads=1
```

The expected result on the checked revision is failure of the promoted
forms, with original forms passing. The installer only adds synthetic tests
and the test-only proof; no upstream production operation is changed. The
public repro is extracted from the locally tested harness and retains the
original guest/oracle attribution. Independent review is requested before
generalizing or enabling the optimization. No scalar/reconvergence/writelane/
vote-loop/f32 fix is implemented in this branch.
