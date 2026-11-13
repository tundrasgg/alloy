# State

The board. Written only by the lead. Rules: `agent/protocol/tasks.md`.

## Now

- Milestone: M0, Foundation (`plan/milestones.md`)
- Milestones exited: none

## Next free ids

| Series | Next |
|---|---|
| Task `T-` | T-0082 |
| Decision `D-` | D-0032 |
| ADR | 0062 |
| Research `R-` | R-0009 |
| Brief `PB-` | PB-0002 |

ADR 0036, ADR 0042, ADR 0046 to ADR 0049, ADR 0051 to ADR 0061, R-0001 to R-0008, and PB-0001 are allocated to the tasks below.

## Board

| Task | Type | Role | Status | Waits on | Title |
|---|---|---|---|---|---|
| T-0001 | build | developer | done | none | Bootstrap the Cargo workspace and the CI gate |
| T-0002 | build | developer | done | none | Implement repository policy lints in check-repo |
| T-0003 | research | researcher | done | none | Research deterministic simulation options in Rust |
| T-0004 | research | researcher | done | none | Research Cloud Hypervisor against ARFC 0011 |
| T-0005 | research | researcher | done | none | Research NAT behaviors and define lab presets |
| T-0006 | spec | architect | done | none | Specify the alloy-rt and alloy-seam interfaces |
| T-0007 | interface | developer | done | none | Define the alloy-rt and alloy-seam interface crates |
| T-0008 | verify | verifier | active | none | Write conformance suites for the runtime seams |
| T-0009 | build | developer | cancelled | none | Build the alloy-sim deterministic simulator |
| T-0010 | verify | verifier | blocked | T-0077 | Write determinism and fault tests for the simulator |
| T-0011 | build | developer | queued | T-0009, T-0021 | Build the NAT models in alloy-natlab |
| T-0012 | verify | verifier | queued | T-0007, T-0021 | Write NAT model tests and the matrix file |
| T-0013 | product | product | done | none | Write the brief for init, join, and first login |
| T-0014 | research | researcher | done | none | Research crates for consensus, storage, and crypto |
| T-0015 | spec | architect | done | none | Record repository hosting and CI runners in ADR 0037 |
| T-0016 | build | developer | done | none | Run cargo xtask ci in GitHub Actions |
| T-0017 | process | architect | done | none | Adopt Conventional Commits for commits on main |
| T-0018 | build | developer | done | none | Check commit messages on main in xtask |
| T-0019 | research | researcher | done | none | Research licenses inside the Cloud Hypervisor UEFI images |
| T-0020 | spec | architect | done | none | Resolve the per-guest seccomp clause in ARFC 0011 |
| T-0021 | spec | architect | done | none | Resolve NAT spec issues found by R-0003 |
| T-0022 | spec | architect | done | none | Apply PB-0001 to ARFC 0002 and ARFC 0019 |
| T-0023 | spec | architect | done | none | Specify user access keys and the local admin path |
| T-0024 | spec | architect | queued | T-0080 | Fix the default mesh port and TCP 443 sharing |
| T-0025 | process | architect | done | none | Let parallel tasks share append-only index files |
| T-0026 | spec | architect | done | none | Make the ARFC 0003 handshake implementable on Noise libraries |
| T-0027 | process | architect | done | none | Align core delegation and the lead role with shared index files |
| T-0028 | spec | architect | done | none | Fix the Landlock clauses in ARFC 0011 section 2 |
| T-0029 | spec | architect | done | none | Record the install script location in ADR 0044 |
| T-0030 | spec | architect | done | none | Fix the DH rate limit and cookie lifetime in ARFC 0003 |
| T-0031 | spec | architect | done | none | Fix the review findings on the T-0021 NAT spec changes |
| T-0032 | process | architect | done | none | Define the inputs of a second round for every role |
| T-0033 | build | developer | done | none | Never cancel CI runs on main |
| T-0034 | research | researcher | done | none | Research browser support for Ed25519 TLS certificates |
| T-0035 | research | researcher | done | none | Research generating the console's TypeScript API client |
| T-0036 | build | developer | done | none | Keep check-repo green on main with plan files in flight |
| T-0037 | build | developer | done | none | Run the check-repo path lock in CI |
| T-0038 | spec | architect | done | none | Specify probe bytes and probe replay protection in ARFC 0003 |
| T-0039 | spec | architect | cancelled | none | Bound probe sets and return path moves in ARFC 0004 |
| T-0040 | spec | architect | done | none | Bound lineage state and close gaps in ARFC 0007 section 3.9 |
| T-0041 | spec | architect | done | none | Use a browser-compatible key type for the API CA |
| T-0042 | build | developer | done | none | Close the check-repo gaps found in review |
| T-0043 | spec | architect | done | none | Settle the path class rule and bound return path moves |
| T-0044 | spec | architect | done | none | Make access key recovery and deleted-key requests well defined |
| T-0045 | spec | architect | queued | D-0028 | Close join, local admin, and API CA gaps in ARFC 0002 and 0019 |
| T-0046 | build | developer | done | none | Enforce direct-io with Clippy on resolved paths |
| T-0047 | spec | architect | cancelled | none | Protect the last cloud-admin and mark ADR 0041 superseded |
| T-0048 | research | researcher | done | none | Research the rustls provider and a P-256 signing source |
| T-0049 | process | architect | done | none | Fix the P-256 rules in the crypto engineering file |
| T-0050 | spec | architect | done | none | Specify the alloy-sim world and scenario interface |
| T-0051 | build | developer | cancelled | none | Run the commit message check in CI |
| T-0052 | spec | architect | done | none | Make the local admin a full break-glass administrator |
| T-0053 | build | developer | done | none | Parse commit trailers only from the last paragraph |
| T-0054 | build | developer | done | none | Build alloy-wire for AL handshake and transport messages |
| T-0055 | verify | verifier | queued | T-0064 | Write conformance vectors for AL handshake and transport bytes |
| T-0056 | process | architect | done | none | Let parallel tasks add crates to the shared workspace files |
| T-0057 | spec | architect | done | none | Keep stored seam configuration readable across releases |
| T-0058 | process | architect | done | none | Align core and lead rules with shared workspace files |
| T-0059 | spec | architect | done | none | Discard sessions a failed handover's successor touched |
| T-0060 | spec | architect | cancelled | none | Settle the open fragment and queue details in ARFC 0003 |
| T-0061 | build | developer | done | none | Add fuzz targets for every alloy-wire decoder |
| T-0062 | build | developer | done | none | Allow thiserror and proptest in the dependency allowlist |
| T-0063 | spec | architect | done | none | Shrink the AL transport header to 25 bytes of overhead |
| T-0064 | build | developer | review | none | Rework alloy-wire for the compact transport header |
| T-0065 | spec | architect | done | none | State break-glass reach into tenant data accurately |
| T-0066 | spec | architect | done | none | Close the open stored-binding and interface gaps in ARFC 0001 |
| T-0067 | spec | architect | done | none | Settle timeout start and path joining in ARFC 0005 |
| T-0068 | build | developer | review | none | Close the direct-io gaps and vet the Clippy fixture's crates |
| T-0069 | build | developer | done | none | Property-test the seam config codec and tidy the crates |
| T-0070 | spec | architect | done | none | State plainly that root on a control node reads all tenant data |
| T-0071 | process | architect | done | none | Record the NCSA exception for libfuzzer-sys in ADR 0060 |
| T-0072 | build | developer | cancelled | none | Run the commit message check in CI from the current cut |
| T-0073 | process | architect | done | none | Adopt lean task sizing and review rules |
| T-0074 | build | developer | done | none | Create alloy-crypto with BLAKE3 content hashing |
| T-0075 | build | developer | review | none | Build the alloy-sim executor, runs, and trace |
| T-0076 | build | developer | queued | T-0075 | Build alloy-sim hosts, mem runtime, and faults |
| T-0077 | build | developer | queued | T-0076 | Build alloy-sim networks, UDP, and the echo scenario |
| T-0078 | build | developer | queued | T-0077 | Build alloy-sim mem streams |
| T-0079 | build | developer | queued | T-0068 | Run the commit check inside cargo xtask ci |
| T-0080 | spec | architect | active | none | Replace path-move heuristics with verify-before-move |
| T-0081 | build | developer | queued | T-0064 | Update the alloy-wire fuzz targets to the compact layout |

## First dispatch

Dispatched: T-0001, T-0003, T-0004, T-0005, T-0013, T-0014, T-0015, T-0017.

## Still to cut for M0

- `os` implementations of the runtime seams, with their verify pair.
- Simulator checkers: linearizability, isolation, durability (ARFC 0005 section 3.6).
- Real-mode NAT scripts and the simulated versus real comparison (ARFC 0005 section 3.5). Include a check that nftables and namespaces can build every R-0003 preset, especially endpoint-independent filtering with a dependent mapping, a separate timer per filter entry, and outbound-only refresh.
- The path lock and banned pattern fixtures for milestone exit criterion 4, if T-0002 leaves any out.

## Process follow-ups

- Which branch a different role starts from in round 1 of a task whose branch another role owns (a `test-issue` ruling). From T-0032.
- `.claude/harness.md` binds "remove the earlier workspace" before a later round to the harness's worktree cleanup. From T-0032.

## Still to cut from T-0006

- T-0007 adds `blake3` (through `alloy-crypto`) to the `deny.toml` allowlist, with the license check R-0004 asks for.
- Spec before the crates that need them: blocking-work offload in `Spawn` (Argon2), Unix peer credentials in `Net` (after the `SO_PEERCRED` research), socket and device handover by file descriptor (ARFC 0017 clause 3.5), and where process spawning lives for the `hypervisor` and `sandbox` seams.

## Still to cut for M3

- Measure on a real KVM host: Cloud Hypervisor create to first guest instruction, migration downtime with vhost-user backends at 100 MiB/s dirty rate, and Landlock behavior with vhost-user socket paths (R-0002, Unknown). Needs a KVM host in the task's environment. Also confirm `Seccomp: 2` on every Cloud Hypervisor thread before the first guest instruction, and migration over a relayed Unix socket (ARFC 0011 clauses 2.3 and 2.4).
- Spec: ARFC 0011 section 3.5 says live migration crosses the VMM sandbox only through a Unix socket in the guest directory (T-0020 follow-up).
- Verify: `tests/conformance/hypervisor` cases for ARFC 0011 clauses 2.1 to 2.7, placed so the `mem` implementation can pass them or is exempt by stated rule (T-0020 review F5). Clause 2.5 tests check the thread that applies the rule set and the threads it starts afterwards, not every TID (T-0028).
- Research: unpack the `ch-97eeb7b09` CLOUDHV images to confirm their module lists, and read the LZMA SDK and `Logo.bmp` licenses (R-0005, Unknown).

## Still to cut for M1 and M2

- From `plan/reports/T-0022.architect.1.md` (cut after T-0022 and T-0023 merge): ARFC 0007 accepts client-generated access key secrets from `ExchangeLoginCode`; ARFC 0006 heartbeat interval under the 30 s `unreachable` rule; ARFC 0017 next minor release across 0.x to 1.0; PB-0001 wording for unreachable and revoked; a command that removes a one-node cluster; proto definitions for the new calls in the first `alloy-proto` task.
- From `plan/reports/T-0022.architect.2.md` (cut after T-0022 merges): I-22 versus the access key secret returned by ARFC 0007 clause 3.9.4, change one; ARFC 0008 clause 3.7 names `ExchangeLoginCode` and the local admin socket; glossary entries for advertise address and context; PB-0001 wording for `key_in_use`, `login_code_voided`, and non-local advertise addresses; a pool brief and a command to take a disk out of a pool so `node leave` works on storage nodes; `--api-port` on `join` is in T-0024.
- From `plan/reports/T-0040.architect.1.md` (cut after T-0022 merges): ARFC 0019 revokes a context's stored key when a login stores another user's key (F6); ARFC 0002 bounds the login code table and keeps each unspent code's issuer, including the access key behind an issuing session token; state the meaning of `key_id` in the token payload; `tests/conformance/authn` cases listed in that report.
- From `plan/reports/T-0023.architect.1.md` (cut after T-0022 merges and T-0023 review): the issues on ARFC 0008, 0002, 0019, 0006, and 0020, the last `cloud-admin` binding, PB-0001 local admin errors, the `SO_PEERCRED` research question, glossary terms, and `tests/conformance/authn` cases.

- Verify: a fixed `IKpsk2` handshake vector for ARFC 0003 sections 3.1 and 3.3 (keys, psk, message bytes, `i2r`, `r2i`, `salt`) in the ARFC 0003 conformance suite (T-0026 follow-up).
- Spec (with the hybrid handshake ARFC, M10): how a node learns a peer supports a later `version` or the hybrid flag before sending it, since version 1 drops both silently (T-0026 re-review note F2).
- Verify: ARFC 0003 section 6 operation counts per packet through `alloy-crypto` on the simulator, and the 3.1 cookie validity across a secret rotation (T-0030).
- Verify and recut: T-0011, T-0012, and conformance suites for the new ARFC 0004 musts after T-0031 merges (answer count and size, probing window, challenge placement and lifetime, 64 outstanding challenges, 5 s home keepalive).
- Spec: ARFC 0008 defines the browser-callable protocol so section 3.10 holds for the console; resolve `prost` in `agent/eng/deps.md` against connect-rust's `buffa` (R-0007).
- Research: observe a Safari TLS handshake with an Ed25519 chain on Apple hardware, and check previous-major Chromium and Firefox in real browsers (R-0006 follow-up).
- Build: `check-repo` rejects all of `std::process` outside `alloy-rt` and `alloy-sim`, which includes `ExitCode` and `exit`; the first binary crate needs a narrower pattern or an exit path through `alloy-rt`. `banned-patterns` scans only `crates/`, not `tests/` or `fuzz/` (T-0042).
- From `plan/reports/T-0043.architect.1.md` (after T-0038 and T-0043 merge): ARFC 0003 section 3.6 lets an anchor's reflection answer carry the observed source endpoint and a challenge; ARFC 0004 states when an anchor considers a home session closed; consider a per-node cap on probes toward unverified advertised candidates; ARFC 0005 section 3.4 states matrix link delays inside ARFC 0004 section 3.2's bounds; T-0011, T-0012, and conformance suites take the new ARFC 0004 musts.
- Spec (after T-0038 and T-0043 merge): ARFC 0004 section 8 and ADR 0039's consequence line restated against probe replay protection in ARFC 0003 section 3.6; the relay envelope in ARFC 0003 section 3.7 gains the `src` that section 3.1's cookie needs; ARFC 0004 section 3.5 says whether path MTU probes travel inside relay envelopes; ARFC 0003 section 7 throttles probe floods that name real node ids. Verify: ARFC 0003 section 3.6 conformance cases listed in `plan/reports/T-0038.architect.1.md`.
- Ruling before the first task that builds TLS: `rustls` providers draw key shares and hello randoms from their own generator, which `agent/eng/crypto.md` rule 4 forbids and which breaks I-20 under the simulator; either an exception to rule 4 or a provider fed from `alloy-rt` (T-0049).
- Spec: ARFC 0008 clause 3.8 names the ACME account key and its type, and ARFC 0002 section 2's RFC 6979 rule covers it, as ADR 0053 decides; ARFC 0002 and 0008 `Decisions` headers list 0053 (T-0049).
- From T-0052 (after it merges): I-02 in `agent/domain/invariants.md` names the break-glass action as the second way across an org boundary; a spec task names the `iam` methods for users, roles, and policies and what `cloud-admin` allows, before any build task cites ARFC 0007 clause 3.10.7; PB-0001 mentions the `local-admin-used` warning; `tests/conformance/authn` cases listed in `plan/reports/T-0052.architect.1.md`.
- Interface (after T-0007 and T-0057 merge): add ARFC 0001 clause 3.19 to `alloy-seam` (`ConfigCodec`, `register_config`, `config`, `configs`) and the `PartialEq` bound on `SeamConfig`, with its verify pair.
- From T-0057 (after it and T-0059 merge; the ARFC 0017 clause 3.7 part is in T-0066): ARFC 0017 clause 3.10 names the upgrade conformance suite; ARFC 0006 clause 3.10 says how a backup restore handles versioned state across releases.
- Build: a fuzz target for `ConfigValue::from_bytes` in `alloy-seam` (T-0007 review, T-0069).
- Testing rule: crates that use `proptest` set a fixed seed and no time-based shrink limit, since its `std` feature reads `Instant::now()` and OS entropy by default (T-0062 review note F1); state it in `agent/eng/testing.md` by a `process` task before the first crate adopts `proptest`.
- From T-0066: decide whether consensus groups refuse binding writes from releases older than `R` during the finalization check (the one case that still stalls finalization until a resource is rebound); ARFC 0006 section 2.2 and ARFC 0002 section 2 cross-reference ARFC 0001 clause 3.13 and ARFC 0017 clause 3.11; the upgrade conformance item extends `tests/conformance/upgrade/` from ARFC 0017 clause 3.12. At merge, ARFC 0017's `Decisions` line combines T-0059's and T-0066's: `0029, 0056, 0057, 0059`.
- Note (T-0059 re-review F1): after a failed handover, a peer whose only session was made by the successor gets no re-handshake and waits up to 120 s.
- Notes from T-0066 re-review (fold into the next ARFC 0001 or 0017 task): ARFC 0017 clause 3.12 lacks scenarios for four rules added in T-0066 round 2 (F1); the clause 3.11 conclusion about written running releases (F2); a global-group recover from a node without the switch re-allows rollback (F3).
- Notes from T-0074 review (fold into the next task on these files): restore the space before `reason` in the `alloy-rt` entry of `deny.toml`; `alloy-crypto`'s `Hasher` doc overstates its buffer (one 64-byte block plus a chaining value).
- Notes from T-0063 review (fold into the next ARFC 0003 task): an unbound VNI idle across a rekey loses its first packets (F1); the 4-sessions-per-peer cap can discard the working session during handshake retries (F2); bind replies reveal which VNIs a node hosts to any session peer (F3); the 2^31 stop rule does not match session choice (F4).
- Tooling note: `cargo xtask ci` does not build `fuzz/`, so T-0064 broke the fuzz targets without the gate noticing; add a fuzz build check in the next `xtask` task.
- Process notes (fold into the next process task): `agent/protocol/spec-change.md`'s behavior-change row matches the lean review rule; `agent/roles/reviewer.md` names interface, spec, and process tasks as review inputs (T-0073).
- Process: a task that supersedes an ADR needs that ADR's status line and its index row in `touches`; the shared index rule has no append-only way to mark a superseded row (T-0044).
- Spec: ARFC 0014 maps native errors, including `Unauthenticated`, to S3 errors; add `Unauthenticated` to `proto/alloy/common/errors.proto` when it is written (T-0044).
- Process: a dependency policy for npm packages, since `deny.toml` covers Cargo only (T-0035).
- Spec (M6): a key type for the ingress ACME certificates in ARFC 0010 clause 3.6 (T-0041).
- The task that wires openraft into `alloy-rt` includes a seed replay test. From R-0004.
- If LMDB or `blake3` are adopted, an ADR adds OpenLDAP and CC0 to the license allowlist. From R-0004.
- Product: replace `<download-host>` in PB-0001 with the ADR 0044 URLs.
- Build: release automation attaches `install.sh` and `install.ps1` to every release (ADR 0044).
- Research: should the 25 s keepalive drop or adapt for NATs with UDP timeouts under 25 s (T-0021 follow-up).
- Spec: ARFC 0004 section 8 states the probe reflection bound from section 3.2 (T-0021 follow-up).

## Pending pivotal questions

None.
