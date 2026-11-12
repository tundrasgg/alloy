# ADR 0019: The control plane is Raft groups, with soft state outside consensus

- Status: accepted
- Subject: control plane
- Specs: arfc-0006

## Context

Desired state needs linearizability. Heartbeats and measurements do not, and would flood a log.

## Decision

A global and a per-region Raft group hold resources. Heartbeats, path data, and usage samples are leased soft state. Voter sets size themselves from 1 to 5.

## Consequences

- A one-node cluster is a group of one and grows without ceremony.
- State size per group is bounded, so scale comes from regions and from keeping hot data out.

## Alternatives rejected

- One external database: another system to deploy, which breaks the one-command start.
