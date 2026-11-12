# ADR 0007: Specifications are law

- Status: accepted
- Subject: process
- Specs: all

## Context

Fast parallel implementation diverges unless one source defines behavior.

## Decision

ARFCs in `spec/` define all observable behavior. Code that differs is a bug. A wrong spec is changed through the spec change protocol before code changes.

## Consequences

- Every 'must' has a conformance test.
- Developers never pick an interpretation silently.

## Alternatives rejected

- Code as the specification: nothing to verify against.
