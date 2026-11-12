# ADR 0033: Conformance tests belong to the verifier role

- Status: accepted
- Subject: process
- Specs: none

## Context

An implementer who writes its own acceptance tests encodes its own mistakes.

## Decision

Conformance, simulation, NAT lab, and system tests are written from the spec by the verifier role, which does not read the implementation. Those paths are locked against build tasks in CI.

## Consequences

- A wrong implementation fails tests it did not influence.
- Interfaces must freeze before build and verify tasks start.

## Alternatives rejected

- Developer-written acceptance tests: fast and self-confirming.
