# ADR 0067: All I/O goes through alloy-rt, and the simulator gates work after M11

- Status: accepted
- Subject: engineering
- Specs: arfc-0005

## Context

ADR 0008 built the simulator and NAT lab before any feature. D-0034 put M11, a first deployment on real hardware, above every milestone, and D-0044 paused the simulator track (T-0077, T-0078, T-0010) until a rung needs it. `alloy-sim` on main has its executor, seeded scheduler, virtual clock, disk model, and crash and restart, but no networks, streams, or NAT lab. This ADR supersedes ADR 0008 so the decision matches the plan.

## Decision

1. Kept from ADR 0008 unchanged: all I/O is behind `alloy-rt` traits, a seeded single-threaded simulator implements them (ADR 0036), and lint rejects direct calls to the clock, network, disk, or randomness.
2. The simulator no longer comes before features. Until M11 exits, rungs are built and checked on real hardware, and simulator work is done only where a rung needs it.
3. Until M11 exits, what runs on the simulator is what `alloy-sim` on main supports: seeded tests on its executor, clock, disk, and crash and restart, such as those of `alloy-id`, `alloy-link`, and `alloy-state`. They stay in `cargo xtask ci`. Behavior that needs a simulated network or NAT is checked by the rung scripts under `tests/e2e/`.
4. A crate built before the simulator is complete must still:
   - make no direct I/O: every use of the clock, randomness, the network, the disk, or task spawning goes through the five `alloy-rt` seams (ARFC 0005 section 2), and `cargo xtask ci` passes its lint;
   - be deterministic for a given input sequence: given one seed and the same result from every `alloy-rt` call, it makes the same calls in the same order and returns the same outputs. Where its tests run on `alloy-sim`, two runs with one seed give the same trace hash (ARFC 0005 clause 3.2.5).
5. The simulator gates work at three points:
   - After M11 exits, a task that changes consensus or replication (ARFC 0006) or a mesh path (handshake, rekey, path selection, relay, or NAT traversal; ARFCs 0003 and 0004) is done only when its behavior passes seeded scenarios on `alloy-sim`, in addition to its own acceptance.
   - No milestone other than M11 exits before M0 exits.
   - No release is published as stable before M0 exits and the simulation exit criteria of every milestone the release contains pass.

## Consequences

- M11 runs on mesh and consensus code with no simulated network faults. Bugs that need rare interleavings may first show on real machines, and are fixed forward.
- Crates built now run on `alloy-sim` later without a rewrite, because rule 4 binds them from their first commit.
- After M11, the simulator network layer, streams, T-0010, and the NAT lab come before the first consensus or mesh path task can be done.
- The M11 rung scripts under `tests/e2e/` are the one test layer above unit tests that does not run on `alloy-sim`.

## Alternatives rejected

- Keep ADR 0008 and finish the simulator and NAT lab first: delays the deployment the pilot put above every milestone (D-0034).
- Drop the simulator: loses the reproducible fault coverage that real-machine tests cannot reach (ADR 0008).
- Allow direct I/O before the simulator and port later: a rewrite per crate, and code never run under a seed.
