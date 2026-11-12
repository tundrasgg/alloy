# ADR 0028: The audit log is hash chained and commits with the change

- Status: accepted
- Subject: control plane
- Specs: arfc-0018

## Context

A multi-tenant system needs a record of changes that cannot be quietly altered, including by its own administrators.

## Decision

Every mutating call writes an audit record in the same transaction as the change. Records chain by hash per organization, with signed checkpoints.

## Consequences

- No change exists without a record.
- Tampering is detectable by recomputing the chain.

## Alternatives rejected

- Best-effort audit logging after the fact: gaps exactly when they matter.
