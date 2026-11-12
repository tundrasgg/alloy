# Specifications

ARFC stands for Alloy RFC. Each ARFC is the normative design of one subsystem.
The rules for writing and changing them are in `agent/protocol/spec-change.md`,
and the template is `agent/templates/arfc.md`.

## Index

| ARFC | Title | Status | Milestone |
|---|---|---|---|
| [0000](arfc-0000-architecture.md) | Architecture | accepted | all |
| [0001](arfc-0001-runtime-seams.md) | Runtime seams | accepted | M0 |
| [0002](arfc-0002-identity-and-join.md) | Identity and join | accepted | M1, M2 |
| [0003](arfc-0003-mesh-wire-protocol.md) | AL, the mesh wire protocol | accepted | M1 |
| [0004](arfc-0004-path-manager.md) | Path manager and relay | accepted | M1 |
| [0005](arfc-0005-simulator-and-natlab.md) | Runtime, simulator, and NAT lab | accepted | M0 |
| [0006](arfc-0006-control-plane.md) | Control plane | accepted | M2 |
| [0007](arfc-0007-tenancy-and-iam.md) | Tenancy and IAM | accepted | M2, M5 |
| [0008](arfc-0008-native-api.md) | Native API | accepted | M2 |
| [0009](arfc-0009-vpc-networking.md) | VPC networking | accepted | M3 |
| [0010](arfc-0010-public-addressing.md) | Public addressing, ingress, and egress | accepted | M6 |
| [0011](arfc-0011-compute.md) | Compute | accepted | M3, M8 |
| [0012](arfc-0012-storage-core.md) | Storage core | accepted | M4 |
| [0013](arfc-0013-block-volumes.md) | Block volumes | accepted | M4 |
| [0014](arfc-0014-object-storage.md) | Object storage | accepted | M7 |
| [0015](arfc-0015-container-registry.md) | Container registry | accepted | M7 |
| [0016](arfc-0016-ec2-compatibility.md) | EC2 compatibility | accepted | M9 |
| [0017](arfc-0017-upgrades-and-versioning.md) | Upgrades and versioning | accepted | M10 |
| [0018](arfc-0018-observability-and-audit.md) | Observability and audit | accepted | M2, M5 |
| [0019](arfc-0019-cli-and-console.md) | CLI and console | draft | M2, M9 |
| [0020](arfc-0020-security-model.md) | Security model | accepted | all |

## Reading order for a newcomer

0000, 0020, 0001, 0005, then the ARFC of the subsystem at hand.

## What "accepted" means today

No ARFC is frozen. Nothing has been released, so every format and API may still
change through the normal spec change process. Sections with open questions name
the task that closes them. An ARFC freezes section by section as releases depend
on it.

## Numbers

Timeouts, sizes, and limits in the ARFCs are requirements with tests. A number
that proves wrong in practice is changed by a spec change with the measurement
that shows it, not by an implementation that quietly differs.
