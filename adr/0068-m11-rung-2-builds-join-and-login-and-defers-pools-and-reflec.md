# ADR 0068: M11 rung 2 builds join and login, and defers pools and reflection

- Status: accepted
- Subject: operations
- Specs: arfc-0002, arfc-0005, arfc-0006, arfc-0007

## Context

M11 rung 2 runs `alloy init` on one Linux node, `alloy join` on a second Linux node and a Windows client, and `alloy node list` on each, all on one LAN. The T-0111 build stopped on gaps. `init` step 3 creates an org, a project, a user, a pool, and a login code, and none of them has a store. Join tokens have no resource kind. Join step 3 needs reflection, which nothing implements before `alloy-relay`. The local admin socket needs the peer's user id, which `net` lacks. ARFC 0002 section 3.14 needs a durable rename, which `disk-io` lacks (T-0106 F4). A preview cluster never changes release (ARFC 0017 clause 3.13), so whatever rung 2 leaves out, a later rung adds on new clusters with no migration.

## Decision

Rung 2 builds what `alloy node list` on each machine needs, and nothing more, and is released only as preview releases. ARFC 0002 section 3.15, ARFC 0006 section 2.8, and ARFC 0007 section 3.11 state it clause by clause.

- `init` creates the node, the one-voter control plane, the org `default`, the user `admin` with `cloud-admin` and no org role, the join token, and the login code. It creates no project, no pool, and no `disks/0`, and prints nothing about them.
- Identity state is resources of the global group: `JoinToken` in `cluster` (ARFC 0006 section 2.9), and `Org`, `User`, `AccessKey`, `LoginCode`, and `LocalAdminUse` in `iam` (ARFC 0007 section 2.5). Access key ids and secrets get a fixed alphabet.
- Login codes come only from `init` and the local admin, so no key descends from another. The API port serves `ListNodes` and `ExchangeLoginCode`, the local admin socket `ListNodes` and `IssueLoginCode`. Every other method answers `UNIMPLEMENTED`. No join approval, token methods, leave, rotation, session tokens, policies, or standing warnings.
- With one control node and no `key-provider` seam, an access key secret is sealed under that node's at-rest key, which ARFC 0020 clause 3.7 allows.
- No node sends or answers a reflection request. An anchor sends on a UDP session only to the endpoint of the initiation it answered, once a packet authenticated under the session arrives from that endpoint, and never moves it. This replaces the rung 2 consequence of ADR 0042 that rung 2 needs reflection probes. From rung 3, ARFC 0004 clause 3.1.2 applies.
- `net` gains `Stream::peer_uid`: `SO_PEERCRED` on Linux, `Unsupported` elsewhere, since a Windows node never holds `control`. `mem` answers 0. `disk-io` gains `replace`: rename and `fsync` of the directory on Linux and macOS, `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` on Windows, durable on return.

## Consequences

- A rung 2 cluster lives one release. Later rungs add projects, pools, approval, token methods, leave, policies, session tokens, `key-provider`, warnings, and reflection, and may change `init`'s objects without a migration.
- `init` and `alloy login` print only what exists: the exchange returns no project, so the context has no default project.
- The rung 2 check clears machines with an uninstall `--purge --force`, because `alloy node leave` is not built.
- An endpoint change of a node strands its session until the node opens one from the new endpoint. On one LAN that does not happen.
- The rule that refuses another user id on the local admin socket is tested under `os`, since `mem` has only user id 0.

## Alternatives rejected

- `alloy node list` through the local admin socket only: ARFC 0019 clause 3.1.14 does not route it there, and the Windows client and the second Linux node have no socket.
- Build reflection in `alloy-link` for rung 2: it serves only NAT and endpoint moves, which one LAN does not have, and rung 3 builds it with `alloy-relay`.
- Keep access key secrets in clear until `key-provider`: against ARFC 0007 clause 3.8 and ARFC 0020 clause 3.7.
- Give `mem` a settable peer user id: a test-only knob in the seam, for one refusal that `os` tests directly.
