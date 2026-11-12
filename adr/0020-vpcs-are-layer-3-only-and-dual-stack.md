# ADR 0020: VPCs are layer 3 only and dual stack

- Status: accepted
- Subject: network
- Specs: arfc-0009

## Context

Layer 2 emulation across a mesh costs broadcast handling and brings little.

## Decision

VPCs route IP only. The virtual switch answers ARP, neighbor discovery, and DHCP itself. Every ENI has IPv4 and IPv6, and IPv6-only VPCs are allowed.

## Consequences

- No broadcast or multicast between guests in version 1.
- Forwarding is a lookup by VNI and address.

## Alternatives rejected

- Layer 2 overlays: flooding, learning, and loops for a feature few workloads need.
