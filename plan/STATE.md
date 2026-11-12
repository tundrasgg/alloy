# State

The board. Written only by the lead. Rules: `agent/protocol/tasks.md`.

## Now

- Milestone: M0, Foundation (`plan/milestones.md`)
- Milestones exited: none

## Next free ids

| Series | Next |
|---|---|
| Task `T-` | T-0018 |
| Decision `D-` | D-0006 |
| ADR | 0038 |
| Research `R-` | R-0005 |
| Brief `PB-` | PB-0002 |

ADR 0036, ADR 0037, R-0001 to R-0004, and PB-0001 are allocated to the tasks below.

## Board

| Task | Type | Role | Status | Waits on | Title |
|---|---|---|---|---|---|
| T-0001 | build | developer | ready | none | Bootstrap the Cargo workspace and the CI gate |
| T-0002 | build | developer | queued | T-0001 | Implement repository policy lints in check-repo |
| T-0003 | research | researcher | ready | none | Research deterministic simulation options in Rust |
| T-0004 | research | researcher | ready | none | Research Cloud Hypervisor against ARFC 0011 |
| T-0005 | research | researcher | ready | none | Research NAT behaviors and define lab presets |
| T-0006 | spec | architect | queued | T-0003 | Specify the alloy-rt and alloy-seam interfaces |
| T-0007 | interface | developer | queued | T-0001, T-0006 | Define the alloy-rt and alloy-seam interface crates |
| T-0008 | verify | verifier | queued | T-0007 | Write conformance suites for the runtime seams |
| T-0009 | build | developer | queued | T-0007 | Build the alloy-sim deterministic simulator |
| T-0010 | verify | verifier | queued | T-0007 | Write determinism and fault tests for the simulator |
| T-0011 | build | developer | queued | T-0009, T-0005 | Build the NAT models in alloy-natlab |
| T-0012 | verify | verifier | queued | T-0007, T-0005 | Write NAT model tests and the matrix file |
| T-0013 | product | product | ready | none | Write the brief for init, join, and first login |
| T-0014 | research | researcher | ready | none | Research crates for consensus, storage, and crypto |
| T-0015 | spec | architect | ready | none | Record repository hosting and CI runners in ADR 0037 |
| T-0016 | build | developer | queued | T-0001, T-0015 | Run cargo xtask ci in GitHub Actions |
| T-0017 | process | architect | ready | none | Adopt Conventional Commits for commits on main |

## First dispatch

Ready now, with disjoint paths: T-0001, T-0003, T-0004, T-0005, T-0013, T-0014, T-0015, T-0017.
Dispatch all eight in one turn.

## Still to cut for M0

- `os` implementations of the runtime seams, with their verify pair.
- Simulator checkers: linearizability, isolation, durability (ARFC 0005 section 3.6).
- Real-mode NAT scripts and the simulated versus real comparison (ARFC 0005 section 3.5).
- The path lock and banned pattern fixtures for milestone exit criterion 4, if T-0002 leaves any out.

## Pending pivotal questions

None.
