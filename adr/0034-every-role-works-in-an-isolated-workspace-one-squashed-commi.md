# ADR 0034: Every role works in an isolated workspace, one squashed commit per task

- Status: accepted
- Subject: process
- Specs: none

## Context

Parallel agents in one working tree corrupt each other's work.

## Decision

Each dispatched role works in its own workspace on its own branch. The lead squashes each task onto main as one commit with a fixed message format and no tool or vendor trailers.

## Consequences

- Any number of tasks run in parallel when their paths are disjoint.
- History reads as one line per task.

## Alternatives rejected

- A shared working tree: races. Merge commits: noisy history.
