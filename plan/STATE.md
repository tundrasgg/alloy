# State

The board. Written only by the lead. Rules: `agent/protocol/tasks.md`.

## Now

- Milestone: M11, First deployment (`plan/milestones.md`, D-0034), above M0 and M1, which continue in parallel
- M11 rung: 2, join. Rung 1 passed on the real machines: `tests/e2e/rung1.ps1` against `v0.0.1-preview.2`, 81 passed, 0 failed (two Linux nodes, one a Proxmox VE host, and the Windows client)
- Milestones exited: none

## Next free ids

| Series | Next |
|---|---|
| Task `T-` | T-0141 |
| Decision `D-` | D-0069 |
| ADR | 0071 |
| Research `R-` | R-0009 |
| Brief `PB-` | PB-0003 |


## Board

Only tasks that are not done or cancelled. A finished task leaves the board in the commit that finishes it; its file stays in `plan/tasks/` and git keeps its history (D-0059).

| Task | Type | Role | Status | Waits on | Title |
|---|---|---|---|---|---|
| T-0010 | verify | verifier | blocked | T-0077 | Write determinism and fault tests for the simulator |
| T-0045 | spec | architect | queued | D-0028 | Close join, local admin, and API CA gaps in ARFC 0002 and 0019 |
| T-0077 | build | developer | paused | D-0044 | Build alloy-sim networks, UDP, and the echo scenario |
| T-0078 | build | developer | paused | D-0044 | Build alloy-sim mem streams |
| T-0084 | verify | verifier | paused | D-0044 | Run the runtime conformance suites against os |
| T-0113 | verify | verifier | review | T-0136 | Write the M11 rung 2 join and node list check |
| T-0119 | spec | architect | queued | before rung 5 | Keep VNI binds alive across crossed rekeys, record the ADR |
| T-0136 | build | developer | active | none | Build M11 rung 2 end to end: API, init, join, and the CLI |
| T-0137 | spec | architect | fix | none | Specify the al0 host interface on Linux and Windows |
| T-0138 | build | developer | active | none | Build the alloy-path path manager for direct paths |
| T-0139 | spec | architect | fix | none | Specify the console's browser API, serving, and toolchain |
| T-0140 | build | developer | active | none | Build the console shell and first screens on fixture data |
| T-0134 | verify | verifier | review | T-0136 | Update the rung 2 check for the login flow |

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

- Spec (with the hybrid handshake ARFC, M10): how a node learns a peer supports a later `version` or the hybrid flag before sending it, since version 1 drops both silently (T-0026 re-review note F2).
- Verify: ARFC 0003 section 6 operation counts per packet through `alloy-crypto` on the simulator, and the 3.1 cookie validity across a secret rotation (T-0030).
- Recut after T-0077 (D-0048): a natlab build task and its verify pair (replacing cancelled T-0011 and T-0012), and conformance suites for the new ARFC 0004 musts (answer count and size, probing window, challenge placement and lifetime, 64 outstanding challenges, 5 s home keepalive).
- Spec: ARFC 0008 defines the browser-callable protocol so section 3.10 holds for the console; resolve `prost` in `agent/eng/deps.md` against connect-rust's `buffa` (R-0007).
- Research: observe a Safari TLS handshake with an Ed25519 chain on Apple hardware, and check previous-major Chromium and Firefox in real browsers (R-0006 follow-up).
- Build: `banned-patterns` scans only `crates/`, not `tests/` or `fuzz/` (T-0042; the `ExitCode` part is T-0089).
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
- Notes from T-0085 review (fold into the next `alloy-crypto` task): HMAC and HKDF are composed in the crate rather than taken from the `hkdf` and `hmac` crates named in `agent/eng/crypto.md`, with no ADR (F1, fix); moved-from stack copies of a transport key and the ephemeral private key are not erased, against the report's claim (F2, note).
- Notes from T-0063 review (fold into the next ARFC 0003 task): an unbound VNI idle across a rekey loses its first packets (F1); the 4-sessions-per-peer cap can discard the working session during handshake retries (F2); bind replies reveal which VNIs a node hosts to any session peer (F3); the 2^31 stop rule does not match session choice (F4).
- Accepted residuals (D-0028, no further `check-repo` hardening): a direct OS call under `#[cfg(not(clippy))]`, and a build script that sets `CLIPPY_CONF_DIR` or `CLIPPY_ARGS` for its own crate, both bypass the Clippy ban; code review catches deliberate evasion (T-0068 review F1, F2).
- From T-0008 (fold into the next ARFC 0005 task): cap the path MTU at 65,535 or `max_payload` at 65,507/65,527 (clause 3.1.8); the Unix socket permission bits in clause 3.1.9 are not observable through a seam; no deterministic trigger for `PermissionDenied` on a port. Next verify tasks: a `mem` harness for `tests/conformance/rt` after T-0078, and an `os` harness after the `os` implementations; `alloy-rt` exports `OsHost` and `register_os` (section 2.6) in the `os` build task.
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

## M11 notes

- From T-0130 review (fold into the next `alloy-state` task): F1, the generic `Group::update`, `delete`, and `remove` skip the identity kinds' checks (`check_kind`), as T-0114's F1 did for `Node`; and the notes in `plan/reports/T-0130.reviewer.1.md`.

- From T-0111 review (fold into the `alloyd init` and `alloyd join` builds): F1, `decode` drops messages ARFC 0006 section 4.3 says must be answered or accepted; F2, `Requester::receive` takes a reply after its time limit; F3 to F5 (notes), O(N) expiry per request, a missing data directory logged once instead of every 60 s, and the rest in `plan/reports/T-0111.reviewer.1.md`.

- From T-0111 (D-0061): `alloy_id::proto::JoinRequest` still has the old `secret` field and body layout; the `alloyd join` build fixes it with `crates/alloy-id/**` in its touches. T-0112 folds T-0091 F2 and F3.

- D-0060: the next `alloy-api` task closes the evicted-connection hold (T-0118 re-review F1). The log-store kill check (ARFC 0006 section 5.5) needs a way to start a process from a test (T-0108).

- Console (PB-0002): cut a spec task from its `spec-issue` entries (browser auth, wire form, serving and pinning, watches) and then the console build; off the rung path.

- After rung 2: split ARFC 0005 into the runtime seams and the simulator and NAT lab, so runtime readers never load simulator text (pilot, spec size).

- D-0056: before any deployment with an anchor that lacks `control`, close the replayed carried `join_request` (T-0110 re-review F1, with F2 and F3).
- T-0106 review F3 and F4 (owner and mode at creation, Windows write-through rename) and T-0107 review F4 (`redb` files at `0600`) go into the build that implements T-0117's `disk-io` protection operations. T-0107 re-review F1 goes into T-0114.


- `check-repo` validates each `reads` entry as a plain path, so the `path#section` form that T-0096 introduced turns main red. Until an `xtask` task strips the `#section` part before the check, task files list whole paths and name sections in Scope.

- D-0045: the Windows service `alloyd` gets the display name "Alloy Underlay Mesh"; fold into the next packaging task and the next ARFC 0017 task.
- ARFC 0009 names no default IPv4 range for a VPC. Lead's proposal for the rung 4 or 5 spec task: the default VPC takes a `/16` from `10.128.0.0/9` that overlaps no route on any node or client node (ARFC 0017 clause 3.17.3), unless the user gives one.

- From T-0092 and T-0093 (fold into the next ARFC 0017 or packaging task): files installed as root are copied from an extracted tree the invoking user can write (T-0092 review F3); uninstall output omits some removed holder directories (re-review note); the snapshot spec issues and the acceptance command forms in T-0093's report; linux-aarch64, windows-aarch64, and macOS archives are not built yet.

- From T-0091 review (fold into the next `alloy`/`alloyd` task, rung 2): F1, `alloyd` reads join state from a data directory without the ARFC 0002 data directory checks; F2, a `Try:` line can suggest a command that is itself a usage error or does not run as printed (T5); F3 (note), usage errors echo control characters to the terminal unescaped.

- From T-0090 re-review (fold into T-0024, the next ARFC 0004 task): F1, the T-0090 behavior changes need an ADR superseding ADR 0061; F2, when an answer is sealed relative to the 2 s probe bound. T-0024 also takes the default mesh port and excluding `al*` addresses from candidates (T-0088 issues); rung 2 needs it.

- From T-0083 re-review (note): on Windows, 8.3 short names and case variants alias long names in `os` `disk-io`, so `list` omits names `open` accepts; fold into the next `alloy-rt` task.

- From T-0088 re-review (fold into the next ARFC 0002 or 0017 task): F1, the Windows data directory has no rule for its ancestors; F2, nothing records a non-default data directory for the unit or the uninstall. F3 and F4 are in T-0092.

- Naming (D-0041): a product task maps the public names onto the glossary, CLI help, and console. "Underlay Mesh" names the overlay, while the glossary's "underlay" means the networks beneath it; that task resolves the wording.

- From T-0086 (fold into the next ARFC 0003 task): the handshake nonce is always 24 zero bytes in `IKpsk2`, so its byte order is unobservable; section 3.3 erasure can only be checked by proxies; add probe key vectors once section 3.6 sealing exists.
- From T-0079 review (note): a dependency added to `alloy-wire` must also update `fuzz/Cargo.lock`, which the `fuzz` gate step does not name when it fails; fold into the next `xtask` task.
- From T-0088 report: no default mesh port is fixed (ARFC 0004 section 3.6); ARFC 0004 does not exclude Alloy's own `al*` addresses from candidates; ARFC 0020 section 2.2 omits release publishers as trusted; PB-0001 must match ARFC 0017 clauses 3.14 to 3.16; `agent/eng/deps.md` should name the ADR 0062 exception; no ARFC defines how `alloy-net` opens its host virtual interface (needed before rungs 2 and 3); compute capacity on a Proxmox VE host must not count memory its guests use (before rung 4).

- The Ubuntu node already runs Docker, a game panel's bridge, and Tailscale (100.64.0.0/10). The Proxmox VE node runs production guests. T-0088 states the coexistence rules.
- From T-0087 (fold into the M11 tasks on ARFC 0008 and 0019): the spec issues listed in `plan/reports/T-0087.product.1.md`, glossary entries for "operation" and "command palette", and a PB-0001 check against rules T1 to T30.

## Pending pivotal questions

None.
