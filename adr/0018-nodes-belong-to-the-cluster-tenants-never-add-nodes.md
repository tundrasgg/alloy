# ADR 0018: Nodes belong to the cluster, tenants never add nodes

- Status: accepted
- Subject: tenancy
- Specs: arfc-0002, arfc-0020

## Context

A node sees the traffic and data of the guests it serves.

## Decision

Only cloud admins add nodes. Every node is trusted with what it serves and nothing more. A node can be dedicated to one organization.

## Consequences

- The join token is a cluster-level secret.
- Scoped views limit what a compromised node learns.

## Alternatives rejected

- Tenant-supplied nodes in a shared mesh: a tenant-controlled machine on the trusted side of the boundary.
