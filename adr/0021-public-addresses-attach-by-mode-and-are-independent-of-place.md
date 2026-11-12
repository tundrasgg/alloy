# ADR 0021: Public addresses attach by mode and are independent of placement

- Status: accepted
- Subject: network
- Specs: arfc-0010

## Context

Clusters get public addresses in very different ways, or have only one.

## Decision

Address pools declare an attachment mode: anchor, l2, routed, or bgp. A holder node attracts the traffic and tunnels it to the guest wherever it runs. Replies leave by the same holder.

## Consequences

- Guests move without changing public address.
- A cluster with one address still gets forwards and ingress.

## Alternatives rejected

- Public addresses bound to the guest's node: placement dictated by addressing.
