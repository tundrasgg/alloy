# Workspace map

Crate boundaries. A crate is created by the first task that needs it. Adding,
splitting, or renaming a crate is a class N decision recorded in
`plan/decisions.md` and reflected here in the same commit.

Dependency rule: a crate may depend only on crates in rows above it, and on
`alloy-rt`, `alloy-seam`, and `alloy-crypto`.

| Crate | Kind | Spec | Holds |
|---|---|---|---|
| `xtask` | bin | none | `ci`, `check-repo`, code generation |
| `alloy-seam` | lib | 0001 | Seam registry |
| `alloy-rt` | lib | 0005 | Runtime traits, `os` implementations |
| `alloy-sim` | lib | 0005 | Simulator, `mem` runtime, checkers |
| `alloy-natlab` | lib | 0005 | NAT models and presets |
| `alloy-crypto` | lib | 0003 | The only crate that depends on cryptographic crates |
| `alloy-wire` | lib | 0003 | AL message encoding and decoding, sans-I/O |
| `alloy-link` | lib | 0003 | Handshake and session state machines, sans-I/O |
| `alloy-path` | lib | 0004 | Path manager state machine, sans-I/O |
| `alloy-relay` | lib | 0004 | Anchor services: reflect, relay, signal |
| `alloy-id` | lib | 0002 | Keys, node records, join tokens |
| `alloy-proto` | lib | 0008 | Protocol Buffers definitions and generated code |
| `alloy-state` | lib | 0006 | Resource store, consensus, watches, views |
| `alloy-iam` | lib | 0007 | Principals, credentials, policy evaluation, quotas |
| `alloy-audit` | lib | 0018 | Audit chain |
| `alloy-obs` | lib | 0018 | Metrics, events, export |
| `alloy-store` | lib, bin | 0012 | Disks, placement, chunk store |
| `alloy-blk` | lib, bin | 0013 | Volumes and vhost-user-blk service |
| `alloy-meta` | lib | 0014 | Sharded ordered index |
| `alloy-net` | lib, bin | 0009, 0010 | Dataplane: mesh I/O, virtual switch, holders |
| `alloy-compute` | lib | 0011 | Hypervisor and sandbox seams, guest lifecycle |
| `alloy-imds` | lib | 0011 | Metadata service |
| `alloy-init` | bin | 0011 | PID 1 inside container microVMs |
| `alloy-ctl` | lib | 0006, 0011 | Controllers and scheduler |
| `alloy-s3` | lib | 0014 | S3 front end |
| `alloy-registry` | lib | 0015 | OCI registry |
| `alloy-ec2` | lib | 0016 | EC2 translator |
| `alloy-api` | lib | 0008 | API server |
| `alloyd` | bin | 0000 | Node daemon |
| `alloy` | bin | 0019 | CLI |

Other top-level directories:

| Path | Holds |
|---|---|
| `proto/` | API definitions |
| `tests/conformance/`, `tests/sim/`, `tests/natlab/`, `tests/system/` | Verifier-owned suites |
| `tests/seeds/` | Seeds of past failures, replayed in CI |
| `fuzz/` | Fuzz targets |
| `ui/console/` | Console |
| `docs/` | User documentation |
