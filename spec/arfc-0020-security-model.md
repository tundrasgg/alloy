# ARFC 0020: Security model

- Status: accepted
- Depends on: 0000
- Crates: all
- Decisions: 0017, 0018, 0022

## 1. Purpose

States who Alloy defends against, where the boundaries are, and what is and is
not promised. Every other ARFC's security section refers here.

## 2. Model

### 2.1 Adversaries

| Adversary | Controls | Wants |
|---|---|---|
| hostile tenant | Its own guests, fully, and its own API credentials | Other tenants' data, traffic, or availability, or the host |
| underlay attacker | Any network between nodes: can read, drop, inject, replay, reorder | Traffic contents, impersonation, disruption |
| outside client | Anything reachable from outside: API, ingress, public addresses, anchors | Unauthorized access, denial of service |
| compromised compute node | One node, fully | To extend beyond what that node legitimately serves |
| stolen credential | One principal's credential | To act beyond that principal's grants |

### 2.2 Trusted

The control node quorum, the cluster key, cloud admins acting through audited
paths, and the hardware and kernel of each node. Compromise of these is outside
the model, though the audit log is built to make cloud admin misuse evident.

### 2.3 Boundaries

| Boundary | Enforced by |
|---|---|
| guest to host | Hardware virtualization, one sandboxed VMM per guest, vhost-user backends treating guest input as hostile |
| VPC to VPC | The VNI in the authenticated AL frame, and port checks in the virtual switch |
| org to org | Authorization on every call, scoped views, per-org chunk scopes |
| node to node | AL sessions bound to node records |
| outside to cluster | TLS on APIs, AL authentication on anchors, security groups on public addresses |

## 3. Behavior

3.1 A hostile tenant must not be able to: read or alter another org's data or
traffic, learn another org's resource names or existence, exhaust a shared
resource beyond its quota and fair share, or run code on a host.

3.2 An underlay attacker must not be able to: read mesh traffic, forge or replay
it, join the cluster without a token, or cause a node to do more than a bounded
amount of work per packet. It can observe traffic volume and timing, and it can
block traffic.

3.3 A compromised compute node can read and alter the guests, volumes, and VPC
traffic it legitimately serves. It must not be able to: read VPCs or volumes it
does not serve, obtain other nodes' keys, write control plane state beyond its own
status, or forge audit records. Removing its node record must end its access
within the bound in ARFC 0002.

3.4 A stolen credential grants exactly its principal's policies until revoked.
Revocation takes effect within 5 seconds across the cluster.

3.5 Defaults are closed: new VPCs admit no inbound traffic from outside, new
buckets and repositories are private, new principals have no grants.

3.6 Every process runs with the least privilege that works. Only `alloyd` and the
dataplane hold elevated capabilities, each a named, minimal set recorded in its
ARFC.

3.7 Secrets at rest on a node (node keys, access key secrets, pool keys) are
encrypted under a node key stored with mode `0600`, and under a hardware key
where one is configured.

## 6. Failure behavior

Security checks fail closed: an error while authenticating, authorizing, or
verifying denies the request.

## 8. Security

### Not defended in version 1

- Side channels between guests sharing a core. A per-node setting disables
  simultaneous multithreading sharing between orgs, at a capacity cost.
- A malicious cloud admin with access to a node's hardware.
- Traffic analysis of the mesh.
- Denial of service by an attacker who can saturate a node's uplink.

These are stated in user documentation.

### Process

- Every boundary above has a hostile scenario in `tests/sim` or `tests/system`
  before the milestone that introduces it exits.
- Every parser of untrusted input has a fuzz target that runs in CI.
- Dependencies are checked for advisories on every CI run.
- A change that touches a boundary gets a review that includes an attack attempt,
  per `agent/roles/reviewer.md`.

Upholds I-01 through I-06, I-09, I-15, I-22.
