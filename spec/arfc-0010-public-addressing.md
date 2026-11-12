# ARFC 0010: Public addressing, ingress, and egress

- Status: accepted
- Depends on: 0009
- Crates: alloy-net, alloy-ctl
- Decisions: 0021

## 1. Purpose

Connects VPCs to the outside: public addresses that can be attached to any guest
wherever it runs, shared ingress for clusters with few addresses, and egress.

## 2. Model

An **address pool** is a set of public IPv4 addresses or IPv6 prefixes that a
cloud admin registers, with an **attachment mode** saying how traffic for those
addresses reaches the cluster:

| Mode | How traffic arrives | Holder |
|---|---|---|
| `anchor` | No pool addresses. Uses an anchor's own address | The anchor |
| `l2` | The addresses are on a layer 2 segment a node is attached to | A node on the segment, which answers ARP and neighbor discovery |
| `routed` | An upstream router has a static route for the prefix to a node | That node, or several by ECMP |
| `bgp` | Alloy announces the addresses from holder nodes over BGP | Any node with a BGP session |

Resources:

| Resource | Is |
|---|---|
| public address | One address from a pool, owned by a project, attachable to an ENI |
| NAT gateway | Shared outbound address translation for a VPC |
| forward | A layer 4 mapping from a public address and port to an ENI and port |
| ingress | An HTTP and TLS router: host names and paths to ENIs and ports |

## 3. Behavior

3.1 A public IPv4 address attached to an ENI is a one-to-one translation to the
ENI's private address, done at the holder. The holder tunnels inbound packets over
the mesh to the guest's node. Replies must leave through the same holder.

3.2 Placement of a guest is independent of where its public address is held. A
guest may move between nodes without its public address changing.

3.3 Public IPv6 is not translated. A subnet may be given a public `/64` from a
pool, and its ENIs then have public IPv6 addresses routed through holders.

3.4 Holder failover: in `l2` mode another node on the segment takes over with a
gratuitous ARP and unsolicited neighbor advertisement. In `bgp` mode the
announcement is withdrawn and remaining holders carry the traffic. In `routed`
mode with one node there is no failover and the pool is marked single-holder. In
all modes with more than one holder, failover completes within 3 seconds.

3.5 `anchor` mode is always available. It supports forwards and ingress, and not
attached public addresses.

3.6 An ingress terminates TLS with certificates obtained by ACME for names that
resolve to it, or with uploaded certificates, routes by SNI and host header, and
supports HTTP/1.1, HTTP/2, and WebSocket. Certificates renew at two thirds of
their lifetime.

3.7 A NAT gateway gives a VPC outbound IPv4 through a public address, or through
its holder's own outbound address when it has none. The default VPC's NAT gateway
uses the hosting node's own default route. Port allocation is per destination,
with 64,512 ports per address.

3.8 Egress for a guest with an attached public address uses that address.

3.9 BGP: Alloy speaks BGP-4 with multiprotocol extensions for IPv6, announces `/32`
and `/128` host routes or configured aggregates, supports MD5 and TCP-AO session
authentication, and never accepts routes into VPCs.

3.10 A public address must only be attachable by principals of the project that
owns it, and a released address must not be given to another org for 5 minutes.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `attachment` | `Attachment` | `anchor`, `l2`, `routed`, `bgp`, `mem` | `tests/conformance/attachment` |

## 6. Failure behavior

A public address whose every holder is down is unreachable from outside and its
status says so. The guest and its private connectivity are unaffected. An ingress
with no healthy backend returns 503.

## 7. Limits

1,024 forwards per public address. 1,000 host rules per ingress. 64 holders per
pool. Pools up to a `/16` of IPv4 and a `/40` of IPv6.

## 8. Security

Inbound traffic passes the ENI's security groups after translation. Holders handle
untrusted traffic and are bound by I-04 and I-06.

## 9. Open questions

- Layer 4 load balancer resource across several backends. Product brief to be cut
  in M6.
