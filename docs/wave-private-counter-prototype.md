# Wave-private LDS counter promotion prototype

This explicit analysis/rewrite is compiled for tests only. Normal translation
and the stable paired backend are unchanged. It is independent of the halves
reduction fix by ColorMeBoom and of the scalar/firstlane work on #2633.

The input to the new GPU test is Dathinel's original synthetic 96-thread
program from 561503d, retained in partial_pairs_tests.rs with the existing
PiloteCode harness and attribution. The test no longer constructs the
fixture-specific promoted derivative before analysis. It passes the intact
original program to private_lds::promote, then translates the returned program
through the existing paired selector, validates DXIL, creates a PSO, dispatches
and checks all seven UAVs and guards against the independent CPU oracle.

## Proof boundary

The first version accepts only one-dimensional compute groups, a linear
initializer, a single-entry/self-loop region and a linear publication phase.
Its abstract state evaluates known integer values separately for every present
guest lane. Unknown inputs and unsupported value calculations remain unknown.
Loop-written registers lose all value information before address analysis;
first-iteration constants cannot establish an invariant indirect address.

It requires all of these properties:

- A zero initializer writes exactly one aligned, allocated dword per logical
  wave, from lane zero. Different waves' counter dwords are disjoint.
- The loop contains exactly one LDS operation: no-return ds_add_u32 with the
  invariant contribution one and the same proven counter address. It has no
  other memory effects or barriers. Returning atomics and GDS are rejected.
- Loop-entry EXEC is exactly the set of present lanes. Loop EXEC changes can
  only intersect the current mask; absent or departed lanes cannot reappear.
- The first observation is an own-counter read under full present EXEC.
  Other LDS addresses before the explicit group barrier must be known and
  disjoint from every counter. Peer observations before that barrier are
  rejected. No untracked memory-to-LDS operation or register indexing mode
  may invalidate this proof.
- The added arithmetic cannot clobber live SCC. Six unused scalar words and
  a new vector word must be available. Publication saves/restores EXEC and
  VCC; it never borrows live guest data registers.

The rewrite initializes per-wave scalar state once, replaces each atomic
with an EXEC population count plus wrapping scalar addition, and publishes
once from lane zero before the first own read. Every original unit atomic
contribution is represented. Branch targets are relocated; the loop backedge
skips the new once-only initializer. Existing guest initialization, scratch
accesses, explicit barriers, peer reads and exports are retained.

This preserves the counter's observable values because no other wave can
observe it during the private phase, no atomic return value is consumed, and
wrapping addition of unit contributions commutes. Different waves can make
different numbers of loop visits without an LDS rendezvous in that loop.
The existing backend admission and rendezvous checks still apply afterward.

## Evidence and limits

On RTX 5070 Ti, two controlled cases pass validated execution with clean
D3D12 debug/GBV: the original 96-thread input (counters 160/112), and a variant
with an invariant copied indirect address and different shrinking EXEC masks
(224/144). Each checks 672 values and 56 guards, including peer counters,
departed lanes, opposite-half firstlane and scratch exchange.

CPU analysis accepts counter proofs for 64, 96, 97, 128 and 256 threads plus
the indirect variant. This is not GPU certification of those additional
sizes. Thirteen deliberately unsafe inputs reject: shared counter aliases,
unknown indirect addresses, lane-private words, non-unit additions, nonzero
initialization, scratch aliases, returning atomics, GDS, changing addresses,
missing peer barrier, missing lane-zero initialization, resurrected EXEC
lanes and live SCC. The older eighteen reduction refusals remain refusals.

For the original 96-thread guest, dynamic guest LDS operations go from 658
to 388: 272 unit atomics become two publication stores. The guest's one
explicit barrier remains. The emitted promoted DXIL has 278 instructions,
six static LDS loads, six static LDS stores, seven barrier calls and two
backend auxiliary atomics. These are compiler counts, not GPU transactions.
The rewrite allocates six scalar words and one vector word; driver register
allocation and physical occupancy are unmeasured. The original is still
refused without the explicit pass, so it is not a valid timing baseline.
There is no claimed frame-time, occupancy or physical register improvement.

Before enabling production use: independently review the abstract semantics
and allocation/relocation contract, extend the accepted regions and add
target-specific profiling against a semantically valid executable baseline.
Unknown or more general addends remain rejected. This is not general LDS
promotion, and is not an implementation of firstlane/scalar fixes owned by
other contributors.
