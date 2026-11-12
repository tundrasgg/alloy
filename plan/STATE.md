# State

The board. Written only by the lead. Rules: `agent/protocol/tasks.md`.

## Now

- Milestone: M0, Foundation (`plan/milestones.md`)
- Milestones exited: none

## Next free ids

| Series | Next |
|---|---|
| Task `T-` | T-0022 |
| Decision `D-` | D-0008 |
| ADR | 0040 |
| Research `R-` | R-0006 |
| Brief `PB-` | PB-0002 |

ADR 0036, ADR 0038, ADR 0039, R-0001 to R-0005, and PB-0001 are allocated to the tasks below.

## Board

| Task | Type | Role | Status | Waits on | Title |
|---|---|---|---|---|---|
| T-0001 | build | developer | review | none | Bootstrap the Cargo workspace and the CI gate |
| T-0002 | build | developer | queued | T-0001 | Implement repository policy lints in check-repo |
| T-0003 | research | researcher | active | none | Research deterministic simulation options in Rust |
| T-0004 | research | researcher | done | none | Research Cloud Hypervisor against ARFC 0011 |
| T-0005 | research | researcher | done | none | Research NAT behaviors and define lab presets |
| T-0006 | spec | architect | queued | T-0003, T-0021 | Specify the alloy-rt and alloy-seam interfaces |
| T-0007 | interface | developer | queued | T-0001, T-0006 | Define the alloy-rt and alloy-seam interface crates |
| T-0008 | verify | verifier | queued | T-0007 | Write conformance suites for the runtime seams |
| T-0009 | build | developer | queued | T-0007 | Build the alloy-sim deterministic simulator |
| T-0010 | verify | verifier | queued | T-0007 | Write determinism and fault tests for the simulator |
| T-0011 | build | developer | queued | T-0009, T-0021 | Build the NAT models in alloy-natlab |
| T-0012 | verify | verifier | queued | T-0007, T-0021 | Write NAT model tests and the matrix file |
| T-0013 | product | product | active | none | Write the brief for init, join, and first login |
| T-0014 | research | researcher | active | none | Research crates for consensus, storage, and crypto |
| T-0015 | spec | architect | done | none | Record repository hosting and CI runners in ADR 0037 |
| T-0016 | build | developer | queued | T-0001 | Run cargo xtask ci in GitHub Actions |
| T-0017 | process | architect | done | none | Adopt Conventional Commits for commits on main |
| T-0018 | build | developer | queued | T-0002 | Check commit messages on main in xtask |
| T-0019 | research | researcher | active | none | Research licenses inside the Cloud Hypervisor UEFI images |
| T-0020 | spec | architect | active | none | Resolve the per-guest seccomp clause in ARFC 0011 |
| T-0021 | spec | architect | active | none | Resolve NAT spec issues found by R-0003 |

## First dispatch

Dispatched: T-0001, T-0003, T-0004, T-0005, T-0013, T-0014, T-0015, T-0017.

## Still to cut for M0

- `os` implementations of the runtime seams, with their verify pair.
- Simulator checkers: linearizability, isolation, durability (ARFC 0005 section 3.6).
- Real-mode NAT scripts and the simulated versus real comparison (ARFC 0005 section 3.5). Include a check that nftables and namespaces can build every R-0003 preset, especially endpoint-independent filtering with a dependent mapping.
- The path lock and banned pattern fixtures for milestone exit criterion 4, if T-0002 leaves any out.

## Still to cut for M3

- Measure on a real KVM host: Cloud Hypervisor create to first guest instruction, migration downtime with vhost-user backends at 100 MiB/s dirty rate, and Landlock behavior with vhost-user socket paths (R-0002, Unknown). Needs a KVM host in the task's environment.

## Pending pivotal questions

None.
