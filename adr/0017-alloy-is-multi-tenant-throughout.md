# ADR 0017: Alloy is multi-tenant throughout

- Status: accepted
- Subject: tenancy
- Specs: arfc-0007

## Context

Tenancy cannot be added later without touching every resource and every boundary.

## Decision

Every resource belongs to an organization and a project from the first data model. There is no single-tenant mode. A new cluster has one organization.

## Consequences

- Authorization, quota, metering, and isolation are part of every service from its first task.
- The single-user case is one organization with defaults.

## Alternatives rejected

- Projects only, tenancy later: a rewrite of isolation at the worst time.
