# ADR 0027: Alloy has its own OCI registry

- Status: accepted
- Subject: storage
- Specs: arfc-0015

## Context

Container workloads need images close by, under the same access rules.

## Decision

Alloy implements the OCI Distribution Specification on its own storage, with repositories scoped to projects and access through IAM.

## Consequences

- Images share storage, quota, and policy with everything else.
- Conformance is measured by the OCI suite.

## Alternatives rejected

- Bundling a separate registry server: a second identity and storage model.
