# ARFC 0009: VPC networking

- Status: accepted
- Depends on: 0003, 0004, 0006, 0007
- Crates: alloy-net
- Decisions: 0013, 0020

## 1. Purpose

Gives each tenant private, isolated, dual-stack layer 3 networks that span every
node of the cluster, enforced in the virtual switch on each host.

## 2. Model

| Resource | Is |
|---|---|
| VPC | An isolated network in a project. Has a VNI, an IPv4 range, and an IPv6 `/56` from the cluster's ULA space |
| subnet | A range inside a VPC: one IPv4 prefix and one IPv6 `/64`. Not tied to a zone |
| ENI | An interface: one MAC, a primary IPv4 and IPv6 address, optional secondary addresses, security groups |
| route table | Routes for a subnet: local, peering, NAT gateway, public |
| security group | Stateful allow rules, by protocol, port, and peer (prefix or security group) |
| peering | A link that routes between two VPCs with disjoint ranges |

The **virtual switch** is part of `alloy-net` on every compute node. Each guest
NIC is a port on it. Guest NICs attach by vhost-user, or by a tap device named
`alv<n>` where vhost-user is unavailable.

A new project gets a `default` VPC with one subnet, a default security group that
allows all traffic between its members and all egress, and a NAT gateway.

## 3. Behavior

3.1 VPCs are layer 3. There is no shared broadcast domain. The switch answers ARP
and neighbor discovery for every address in the VPC with the gateway's MAC, and
answers DHCPv4, DHCPv6, and router advertisements itself.

3.2 Forwarding: for a packet from a port, the switch checks the source, applies
egress rules, looks up `(VNI, destination)` in its forwarding table, and sends the
packet in an AL transport frame to the node hosting the destination, or delivers
it locally. The receiving switch applies ingress rules and delivers.

3.3 The forwarding table comes from the node's view. A node holds entries only for
VPCs that have a port on it. In a VPC with more than 10,000 ENIs, entries are
fetched on first use and cached for 60 seconds, with invalidation pushed on
change.

3.4 A port must drop any frame whose source MAC or IP is not one of the ENI's own,
unless the ENI has `source_check: false` set by a principal allowed to set it.

3.5 A packet must never be delivered to a port whose ENI is not in the packet's
VNI (I-01). The VNI is read from the authenticated inner frame, never from
anything a guest controls.

3.6 Security groups are stateful: a permitted flow permits its replies. Rules
apply at both the sending and the receiving host. A rule change applies to new
flows within 1 second, and existing flows that are no longer allowed are cut
within 5 seconds.

3.7 Connection tracking is per port, default 65,536 flows per ENI. At the limit,
new flows are dropped and counted, existing flows continue (I-04).

3.8 DNS: the second address of each subnet is a resolver. It answers
`<instance>.<vpc>.<project>.internal` and the same for containers, with A and AAAA
records, and forwards other names to resolvers set on the VPC, by default the
host's.

3.9 MTU per VPC: `auto` (default: 1,500 when every path among the VPC's nodes
carries it without fragmenting, otherwise the largest size that does, and never
below 1,280), `1500` (always 1,500, using AL fragmentation where needed), or a
fixed value. Guests learn it by DHCP and router advertisement.

3.10 IPv6 is native. Every ENI has an IPv6 address. A VPC may be IPv6-only.

3.11 Unsupported in version 1 and dropped at the port: broadcast beyond ARP and
DHCP, multicast, non-IP EtherTypes.

3.12 Peering routes between two VPCs, including across orgs, when both sides
accept and their ranges do not overlap. Security groups still apply.

3.13 Client nodes. A client node (ARFC 0002 section 3.11) runs no virtual
switch and holds no port. It reaches VPC addresses only through `al0`: every
route it takes to a VPC prefix goes through `al0`, is added by `add_route`
(ARFC 0005 clause 3.1.17), and follows ARFC 0017 clause 3.17.3, and every
packet it sends to a VPC address is one it reads from `al0`. This version
gives a client node no such route: it adds no route through `al0`, and drops
each packet it reads there for an address outside the infrastructure network
(ARFC 0002 section 3.4). The spec task of M11 rung 6 extends this clause with
the prefixes a client node routes, the node and VNI its packets go to, and the
rules that apply to them.

## 6. Failure behavior

A node with a stale view forwards by its last table. A packet for an ENI that has
moved is answered with a redirect by the old host for 30 seconds, then dropped.
`alloy-net` restart: ports reattach and flows resume from saved connection
tracking state within 1 second.

## 7. Limits

16,777,215 VNIs. 100,000 ENIs per VPC. 250 rules per security group, 5 groups per
ENI. 500 routes per table. 32 secondary addresses per ENI.

## 8. Security

Upholds I-01, I-03, I-04. The switch treats all guest traffic as hostile.
