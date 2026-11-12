# Engineering: testing

Alloy is distributed systems code written at speed. Tests are how it stays
correct, and determinism is how tests stay useful.

## Layers

| Layer | Where | Written by | Checks |
|---|---|---|---|
| Unit | `crates/*/src`, `#[cfg(test)]` | developer | Internal logic |
| Property | `crates/*/tests` | developer | State machines against a model |
| Conformance | `tests/conformance/<suite>` | verifier | Every "must" in an ARFC |
| Simulation | `tests/sim` | verifier | Whole clusters under faults, by seed |
| NAT lab | `tests/natlab` | verifier | Connectivity across NAT pairings |
| Fuzz | `fuzz/` | developer, reviewer | Every parser and decoder |
| System | `tests/system` | verifier | Real binaries in network namespaces and VMs |

## Determinism

- Every test above the unit layer runs on `alloy-sim` and takes a seed.
- Same seed, same binary: identical event trace. CI checks this by running a
  sample of scenarios twice and comparing trace hashes.
- No test sleeps, reads the wall clock, binds a real port, or depends on test
  order.
- A failure prints its seed. The seed is added to `tests/seeds/<suite>.txt` in the
  fix, and CI replays that file forever.
- CI runs each simulation scenario on a fixed list of seeds plus a batch of fresh
  ones. A fresh seed that fails is a real failure.

## Faults the simulator injects

Packet loss, duplication, reordering, delay, and corruption. Partitions, one-way
and full. NAT rebinding and mapping expiry. Clock skew and jumps. Process crash
and restart at any await point. Disk write loss after a crash for data that was
not synced, torn writes, read corruption, and full disks. Slow nodes.

## Seams

Every seam has a conformance suite that is generic over the trait. Every
implementation, including the in-memory one the simulator uses, must pass it. A
new implementation is not registered until it does.

## Required for specific code

- Parsers and decoders: a fuzz target, and a property test that
  `decode(encode(x)) == x`.
- State machines: a property test against a simple model.
- Lock-free structures: `loom` tests.
- Consensus and replication: linearizability checking of simulated histories.
- Anything in the crates that allow `unsafe`: Miri on the unit tests.
- Tenant isolation: a hostile tenant scenario for each boundary in
  `spec/arfc-0020-security-model.md`.

## Path lock

`tests/conformance/`, `tests/sim/`, `tests/natlab/`, and `tests/system/` are
written only on verifier branches. `cargo xtask check-repo` fails a build task
branch that changes them.
