# ADR 0006: The lead runs on the build tier, the deep tier is an exception

- Status: accepted
- Subject: process
- Specs: none

## Context

The most capable model is expensive and is not needed for coordination.

## Decision

The lead and five roles use the build tier, the researcher uses the scan tier. The deep tier is used only for small, well scoped, extremely high stakes tasks, at most about once per twenty tasks.

## Consequences

- Tasks must be cut small enough for the build tier.
- `executor: deep` in a task file is visible and reviewable.

## Alternatives rejected

- A deep tier lead: cost with little gain, since the lead reads reports and cuts tasks.
