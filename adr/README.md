# Decisions

One decision per file. ADRs are never edited after acceptance, a changed mind is
a new ADR that supersedes the old one. Rules: `agent/protocol/decisions.md`.
Template: `agent/templates/adr.md`.

Check this index before making a design choice. If the choice is here, follow it
or supersede it.

## Project

| ADR | Decision |
|---|---|
| [0001](0001-alloy-is-licensed-mit-or-apache-2-0.md) | Alloy is licensed MIT OR Apache-2.0 |
| [0002](0002-rust-for-the-system-typescript-for-the-console.md) | Rust for the system, TypeScript for the console |
| [0037](0037-github-hosts-the-source-and-ci-runs-on-github-hosted-runners.md) | GitHub hosts the source and CI runs on GitHub-hosted runners |
| [0044](0044-install-scripts-are-served-from-github-releases.md) | Install scripts are served from GitHub Releases |

## Process

| ADR | Decision |
|---|---|
| [0003](0003-alloy-is-built-by-coordinated-agents-under-rules-in-the-repo.md) | Alloy is built by coordinated agents under rules in the repository |
| [0004](0004-agents-md-is-the-only-instruction-entry-file.md) | AGENTS.md is the only instruction entry file |
| [0005](0005-no-agent-memory-and-nothing-pilot-specific-in-the-repository.md) | No agent memory and nothing pilot-specific in the repository |
| [0006](0006-the-lead-runs-on-the-build-tier-the-deep-tier-is-an-exceptio.md) | The lead runs on the build tier, the deep tier is an exception |
| [0007](0007-specifications-are-law.md) | Specifications are law |
| [0032](0032-repository-files-carry-no-dates-git-is-the-clock.md) | Repository files carry no dates, git is the clock |
| [0033](0033-conformance-tests-belong-to-the-verifier-role.md) | Conformance tests belong to the verifier role |
| [0034](0034-every-role-works-in-an-isolated-workspace-one-squashed-commi.md) | Every role works in an isolated workspace, one squashed commit per task |

## Engineering

| ADR | Decision |
|---|---|
| [0008](0008-deterministic-simulation-comes-first-and-all-i-o-goes-throug.md) | Deterministic simulation comes first and all I/O goes through alloy-rt |
| [0009](0009-every-subsystem-boundary-is-a-runtime-swappable-seam-from-da.md) | Every subsystem boundary is a runtime-swappable seam from day zero |
| [0036](0036-alloy-sim-is-written-in-house-on-the-alloy-rt-seams.md) | alloy-sim is written in-house on the alloy-rt seams |
| [0035](0035-dependencies-must-be-permissively-licensed.md) | Dependencies must be permissively licensed |
| [0054](0054-simulated-worlds-are-network-trees-driven-by-a-control-task.md) | Simulated worlds are network trees driven by a control task |
| [0060](0060-libfuzzer-sys-alone-may-use-ncsa-in-fuzz-targets-only.md) | libfuzzer-sys alone may use NCSA, in fuzz targets only |
| [0062](0062-the-windows-client-may-ship-the-signed-wintun-dll-unmodified.md) | The Windows client may ship the signed Wintun DLL unmodified |

## Architecture

| ADR | Decision |
|---|---|
| [0031](0031-scale-comes-from-lazy-sessions-scoped-views-and-regions.md) | Scale comes from lazy sessions, scoped views, and regions |

## Mesh

| ADR | Decision |
|---|---|
| [0010](0010-alloy-has-its-own-overlay-protocol-and-a-userland-dataplane.md) | Alloy has its own overlay protocol and a userland dataplane |
| [0011](0011-xchacha20-poly1305-with-a-noise-ik-handshake.md) | XChaCha20-Poly1305 with a Noise IK handshake |
| [0012](0012-handshake-replay-protection-uses-a-persisted-counter-not-a-c.md) | Handshake replay protection uses a persisted counter, not a clock |
| [0013](0013-fragmentation-happens-inside-al-never-in-ip.md) | Fragmentation happens inside AL, never in IP |
| [0014](0014-traffic-starts-on-a-relay-and-upgrades-to-a-direct-path.md) | Traffic starts on a relay and upgrades to a direct path |
| [0015](0015-a-cluster-needs-only-one-address-that-all-nodes-can-reach.md) | A cluster needs only one address that all nodes can reach |
| [0016](0016-relay-only-nodes-may-run-workloads-with-a-warning-and-a-cap.md) | Relay-only nodes may run workloads, with a warning and a cap |
| [0030](0030-the-handshake-reserves-room-for-a-post-quantum-key-exchange.md) | The handshake reserves room for a post-quantum key exchange |
| [0039](0039-an-authenticated-probe-is-answered-at-the-source-it-arrived.md) | An authenticated probe is answered at the source it arrived from |
| [0043](0043-the-al-handshake-is-fixed-byte-for-byte-on-noise-ikpsk2.md) | The AL handshake is fixed byte for byte on Noise IKpsk2 |
| [0045](0045-home-sessions-get-a-keepalive-every-5-seconds-of-idleness.md) | Home sessions get a keepalive every 5 seconds of idleness |
| [0046](0046-probes-are-sealed-under-session-keys-with-a-replay-window.md) | Probes are sealed under session keys with a replay window |
| [0049](0049-only-answers-and-bounded-probes-reach-an-unverified-endpoint.md) | Only answers and bounded probes reach an unverified endpoint (superseded by 0061) |
| [0057](0057-sessions-are-used-only-with-current-state-the-peer-holds.md) | Sessions are used only with current state the peer holds |
| [0058](0058-al-transport-packets-carry-25-bytes-of-overhead.md) | AL transport packets carry 25 bytes of overhead |
| [0061](0061-traffic-moves-only-to-an-endpoint-that-echoed-a-challenge.md) | Traffic moves only to an endpoint that echoed a challenge (superseded by 0063) |
| [0063](0063-path-checks-are-held-apart-from-probes-and-traffic-moves-onl.md) | Path checks are held apart from probes, and traffic moves only on an echo |

## Control plane

| ADR | Decision |
|---|---|
| [0019](0019-the-control-plane-is-raft-groups-with-soft-state-outside-con.md) | The control plane is Raft groups, with soft state outside consensus |
| [0028](0028-the-audit-log-is-hash-chained-and-commits-with-the-change.md) | The audit log is hash chained and commits with the change |

## Tenancy

| ADR | Decision |
|---|---|
| [0017](0017-alloy-is-multi-tenant-throughout.md) | Alloy is multi-tenant throughout |
| [0018](0018-nodes-belong-to-the-cluster-tenants-never-add-nodes.md) | Nodes belong to the cluster, tenants never add nodes |
| [0041](0041-users-hold-access-keys-from-login-codes-root-on-a-control-nod.md) | Users hold access keys from login codes, root on a control node is a local admin (superseded by 0050) |
| [0050](0050-users-hold-access-keys-from-login-codes-a-local-admin-can-iss.md) | Users hold access keys from login codes, a local admin can issue and revoke them (superseded by 0055) |
| [0055](0055-root-on-a-control-node-is-a-full-break-glass-administrator.md) | Root on a control node is a full break-glass administrator |

## Api

| ADR | Decision |
|---|---|
| [0026](0026-the-native-api-is-the-source-of-truth-ec2-and-s3-are-compati.md) | The native API is the source of truth, EC2 and S3 are compatibility surfaces |
| [0042](0042-nodes-talk-to-the-control-plane-over-al-clients-to-the-api-o.md) | Nodes talk to the control plane over AL, clients to the API over gRPC |
| [0048](0048-the-api-ca-and-api-server-certificates-use-ecdsa-p-256.md) | The API CA and API server certificates use ECDSA P-256 |
| [0053](0053-p-256-signs-through-rustcrypto-and-rustls-uses-aws-lc-rs.md) | P-256 signs through RustCrypto, and rustls uses aws-lc-rs |

## Network

| ADR | Decision |
|---|---|
| [0020](0020-vpcs-are-layer-3-only-and-dual-stack.md) | VPCs are layer 3 only and dual stack |
| [0021](0021-public-addresses-attach-by-mode-and-are-independent-of-place.md) | Public addresses attach by mode and are independent of placement |

## Compute

| ADR | Decision |
|---|---|
| [0022](0022-one-sandboxed-vmm-process-per-guest-cloud-hypervisor-first.md) | One sandboxed VMM process per guest, Cloud Hypervisor first |
| [0023](0023-containers-run-as-microvms.md) | Containers run as microVMs |
| [0024](0024-nodes-without-hardware-virtualization-do-not-run-mixed-tenan.md) | Nodes without hardware virtualization do not run mixed tenants |
| [0038](0038-seccomp-is-fixed-per-hypervisor-landlock-and-namespaces-per-g.md) | Seccomp is fixed per hypervisor, Landlock and namespaces per guest |

## Storage

| ADR | Decision |
|---|---|
| [0025](0025-alloy-has-its-own-storage-system.md) | Alloy has its own storage system |
| [0027](0027-alloy-has-its-own-oci-registry.md) | Alloy has its own OCI registry |

## Operations

| ADR | Decision |
|---|---|
| [0029](0029-upgrades-are-rolling-with-process-handover-and-a-one-release.md) | Upgrades are rolling, with process handover and a one-release skew |
| [0040](0040-init-and-join-need-no-flags-and-a-one-use-login-code-carries.md) | Init and join need no flags, and a one-use login code carries the first credential |
| [0056](0056-finalizing-an-upgrade-rewrites-stored-seam-bindings.md) | Finalizing an upgrade rewrites stored seam bindings |
| [0059](0059-finalization-covers-new-groups-running-releases-and-re-encoding.md) | Finalization covers new groups, running releases, and re-encoding |
