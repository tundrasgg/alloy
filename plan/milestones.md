# Milestones

A milestone exits when every exit criterion is shown by a command and its output
in a report. The lead records the exit in `plan/STATE.md`.

Milestones are ordered by dependency, not by calendar. Tracks that depend only on
M0 start as soon as M0 exits and run in parallel.

```
M0 ──┬── M1 ── M2 ──┬── M3 ──┬── M6
     │              │        ├── M8
     │              ├── M5   └── M9
     └── M4 ────────┴── M7
                              all ── M10
```

## M0: Foundation

Specs: 0001, 0005.

- Cargo workspace, pinned toolchain, `cargo xtask ci`, `cargo xtask check-repo`.
- `alloy-seam` registry. `alloy-rt` traits with `os` and `mem` implementations.
- `alloy-sim`: deterministic executor, virtual clock, network, disk, fault
  injection, trace hashing, checkers.
- `alloy-natlab`: NAT models and presets, the matrix file, real-mode scripts.

Exit:
1. A 100-host echo scenario produces the same trace hash on two runs with one
   seed, and different hashes on two seeds.
2. One virtual hour of that scenario runs in under 60 seconds.
3. Every NAT preset passes its RFC 4787 behavior tests in the simulator and
   matches its real-mode counterpart.
4. `check-repo` rejects each banned pattern and each path lock violation, shown by
   fixtures.

## M1: Mesh

Specs: 0003, 0004, parts of 0002.

- `alloy-crypto`, `alloy-wire`, `alloy-link`, `alloy-path`, `alloy-relay`.
- `alloy-net` with the `al0` interface. UDP and stream underlays.

Exit:
1. The full NAT matrix passes in simulation: every pairing reaches its expected
   path class within its bound.
2. A sample of the matrix passes with real binaries in network namespaces.
3. Fuzz targets for every AL message run 10 minutes each with no finding.
4. Throughput between two hosts on the reference machine: at least 5 Gbit/s on one
   core with 1,500 byte packets.

## M2: Control plane and join

Specs: 0002, 0006, 0008, skeletons of 0007, 0018, 0019.

- `alloy-state`, `alloy-ctl`, `alloy-proto`, `alloy-api`, `alloy-id`, `alloyd`,
  the `alloy` CLI with `init`, `join`, `login`, `node`, `token`.
- Organizations and projects in the data model. Authorization hook on every call.
  Audit record on every mutation.

Exit:
1. `alloy init` then two `alloy join` commands form a three-voter cluster, in
   simulation and with real binaries.
2. Linearizability checks pass under partitions, crashes, and restarts across
   10,000 seeds.
3. A node receives nothing outside its view, shown by the isolation checker.

## M3: Compute and VPC

Specs: 0009, 0011 (instances).

- Virtual switch, security groups, DNS, DHCP, metadata service, images, scheduler,
  instances on Cloud Hypervisor, node-local volumes sufficient to boot.

Exit:
1. `alloy instance create` with no flags yields a reachable instance.
2. Two instances in one VPC on different nodes behind NAT reach each other.
   Instances in different VPCs cannot. Hostile tenant scenarios for I-01 and I-04
   pass.
3. A stock cloud image configures itself from the metadata service.

## M4: Storage

Specs: 0012, 0013.

- `alloy-store` (disks, placement, PGs, recovery, scrub), `alloy-blk`, volumes,
  snapshots, clones, fencing.

Exit:
1. The durability checker passes across 10,000 seeds with disk loss, node loss,
   partitions, and power loss.
2. Two-writer scenarios never corrupt a volume (I-13).
3. The performance floor in ARFC 0013 holds on the reference machine.

## M5: Tenancy

Specs: 0007, 0018.

- Full policy engine, external identity provider, quotas, metering, audit chains
  and verification, tenant-visible metrics and events.

Exit:
1. Cross-org access scenarios all deny, and denials are indistinguishable from
   absence.
2. `alloy audit verify` detects every injected tamper in tests.
3. Quotas hold under concurrent creation across 10,000 seeds.

## M6: Public networking

Spec: 0010.

Exit: each attachment mode passes its conformance suite, failover completes within
3 seconds, and an ingress serves HTTPS with an automatically issued certificate
against a test ACME server.

## M7: Object storage and registry

Specs: 0014, 0015.

Exit: the pinned S3 test list passes, the OCI conformance suite passes, and index
shard split and merge scenarios pass the consistency checker.

## M8: Containers

Spec: 0011 (containers).

Exit: a container starts from an image in the Alloy registry within the bound in
ARFC 0011, with VPC networking, a volume, and an instance role.

## M9: EC2 surface and console

Specs: 0016, 0019.

Exit: the three pinned EC2 suites pass, and the console covers every tenant
operation the CLI has.

## M10: Hardening

Specs: 0017, 0020, all limits.

Exit: a mixed-version rolling upgrade passes every scenario, a 10,000 node
simulated region meets the latency bounds in ARFCs 0006 and 0011, every boundary
in ARFC 0020 has a passing hostile scenario, and every limit in every ARFC has a
test at the limit.

## E1: First deployment

Pilot direction (D-0034). A vertical slice through M1, M2, and M3 on real
hardware: two Linux compute nodes, one of them a Proxmox VE host, and one Windows
client that is a node with no roles. It takes priority over every other
milestone. Work proceeds in rungs, in order; a rung is done when its check passes
on the real machines and the report quotes the commands and their output, and the
next rung's check is not run before that. Every rung leaves a script under
`tests/e2e/` that reruns its check from scratch, with the machines named only by
parameters.

Rules:
- Alloy coexists with what a host already runs. On a Proxmox VE host it never
  changes the host's bridges, firewall, storage, or existing guests. On any host it
  uses only interfaces, routes, firewall tables, and address ranges it created,
  and none that overlap ones already in use.
- Every client operating system gets the `alloy` CLI and a mesh client that
  reaches VPC addresses. Windows hosts no compute.

Rungs:
1. Install: one command installs `alloyd` on each Linux node and the CLI and mesh
   client on Windows. Uninstall removes all of it.
2. Join: `alloy init` on one Linux node, `alloy join` on the other and on Windows.
   The node list shows all three, healthy.
3. Mesh: all three reach each other over the overlay, and the check shows each
   path as direct or relayed.
4. Compute: creating a VM boots a Linux guest on each Linux node in turn. Both get
   an address by DHCP and reach the internet.
5. VPC: one VM per Linux node on the same VPC ping each other across hosts on
   private addresses.
6. Client: Windows reaches both VMs' private addresses and opens SSH to each.
7. Survival: a rebooted Linux node rejoins, and its VM comes back with no manual
   step.

Exit: all seven checks rerun green, back to back, from a clean state.
