# State

The board. Written only by the lead. Rules: `agent/protocol/tasks.md`.

## Now

- Milestone: M0, Foundation (`plan/milestones.md`)
- Milestones exited: none

## Next free ids

| Series | Next |
|---|---|
| Task `T-` | T-0030 |
| Decision `D-` | D-0010 |
| ADR | 0045 |
| Research `R-` | R-0006 |
| Brief `PB-` | PB-0002 |

ADR 0036, ADR 0040 to ADR 0044, R-0001 to R-0005, and PB-0001 are allocated to the tasks below.

## Board

| Task | Type | Role | Status | Waits on | Title |
|---|---|---|---|---|---|
| T-0001 | build | developer | done | none | Bootstrap the Cargo workspace and the CI gate |
| T-0002 | build | developer | active | none | Implement repository policy lints in check-repo |
| T-0003 | research | researcher | done | none | Research deterministic simulation options in Rust |
| T-0004 | research | researcher | done | none | Research Cloud Hypervisor against ARFC 0011 |
| T-0005 | research | researcher | done | none | Research NAT behaviors and define lab presets |
| T-0006 | spec | architect | queued | T-0021 | Specify the alloy-rt and alloy-seam interfaces |
| T-0007 | interface | developer | queued | T-0001, T-0006 | Define the alloy-rt and alloy-seam interface crates |
| T-0008 | verify | verifier | queued | T-0007 | Write conformance suites for the runtime seams |
| T-0009 | build | developer | queued | T-0007 | Build the alloy-sim deterministic simulator |
| T-0010 | verify | verifier | queued | T-0007 | Write determinism and fault tests for the simulator |
| T-0011 | build | developer | queued | T-0009, T-0021 | Build the NAT models in alloy-natlab |
| T-0012 | verify | verifier | queued | T-0007, T-0021 | Write NAT model tests and the matrix file |
| T-0013 | product | product | done | none | Write the brief for init, join, and first login |
| T-0014 | research | researcher | done | none | Research crates for consensus, storage, and crypto |
| T-0015 | spec | architect | done | none | Record repository hosting and CI runners in ADR 0037 |
| T-0016 | build | developer | active | none | Run cargo xtask ci in GitHub Actions |
| T-0017 | process | architect | done | none | Adopt Conventional Commits for commits on main |
| T-0018 | build | developer | queued | T-0002 | Check commit messages on main in xtask |
| T-0019 | research | researcher | done | none | Research licenses inside the Cloud Hypervisor UEFI images |
| T-0020 | spec | architect | done | none | Resolve the per-guest seccomp clause in ARFC 0011 |
| T-0021 | spec | architect | review | none | Resolve NAT spec issues found by R-0003 |
| T-0022 | spec | architect | active | none | Apply PB-0001 to ARFC 0002 and ARFC 0019 |
| T-0023 | spec | architect | active | none | Specify user access keys and the local admin path |
| T-0024 | spec | architect | queued | T-0021 | Fix the default mesh port and TCP 443 sharing |
| T-0025 | process | architect | done | none | Let parallel tasks share append-only index files |
| T-0026 | spec | architect | active | none | Make the ARFC 0003 handshake implementable on Noise libraries |
| T-0027 | process | architect | done | none | Align core delegation and the lead role with shared index files |
| T-0028 | spec | architect | active | none | Fix the Landlock clauses in ARFC 0011 section 2 |
| T-0029 | spec | architect | done | none | Record the install script location in ADR 0044 |

## First dispatch

Dispatched: T-0001, T-0003, T-0004, T-0005, T-0013, T-0014, T-0015, T-0017.

## Still to cut for M0

- `os` implementations of the runtime seams, with their verify pair.
- Simulator checkers: linearizability, isolation, durability (ARFC 0005 section 3.6).
- Real-mode NAT scripts and the simulated versus real comparison (ARFC 0005 section 3.5). Include a check that nftables and namespaces can build every R-0003 preset, especially endpoint-independent filtering with a dependent mapping, a separate timer per filter entry, and outbound-only refresh.
- The path lock and banned pattern fixtures for milestone exit criterion 4, if T-0002 leaves any out.

## Still to cut for M3

- Measure on a real KVM host: Cloud Hypervisor create to first guest instruction, migration downtime with vhost-user backends at 100 MiB/s dirty rate, and Landlock behavior with vhost-user socket paths (R-0002, Unknown). Needs a KVM host in the task's environment. Also confirm `Seccomp: 2` on every Cloud Hypervisor thread before the first guest instruction, and migration over a relayed Unix socket (ARFC 0011 clauses 2.3 and 2.4).
- Spec: ARFC 0011 section 3.5 says live migration crosses the VMM sandbox only through a Unix socket in the guest directory (T-0020 follow-up).
- Verify: `tests/conformance/hypervisor` cases for ARFC 0011 clauses 2.1 to 2.7, placed so the `mem` implementation can pass them or is exempt by stated rule (T-0020 review F5).
- Research: unpack the `ch-97eeb7b09` CLOUDHV images to confirm their module lists, and read the LZMA SDK and `Logo.bmp` licenses (R-0005, Unknown).
- The next research task adds the R-0005 row to `research/README.md` in its `touches`.

## Still to cut for M1 and M2

- Research then ADR: the `rustls` crypto provider (`aws-lc-rs` needs a C build and license review; `ring` release cadence). From R-0004.
- The task that wires openraft into `alloy-rt` includes a seed replay test. From R-0004.
- If LMDB or `blake3` are adopted, an ADR adds OpenLDAP and CC0 to the license allowlist. From R-0004.
- Product: replace `<download-host>` in PB-0001 with the ADR 0044 URLs.
- Build: release automation attaches `install.sh` and `install.ps1` to every release (ADR 0044).
- Research: should the 25 s keepalive drop or adapt for NATs with UDP timeouts under 25 s (T-0021 follow-up).
- Spec: ARFC 0004 section 8 states the probe reflection bound from section 3.2 (T-0021 follow-up).

## Pending pivotal questions

None.
