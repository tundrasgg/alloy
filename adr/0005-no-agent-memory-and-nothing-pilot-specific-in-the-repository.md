# ADR 0005: No agent memory and nothing pilot-specific in the repository

- Status: accepted
- Subject: process
- Specs: none

## Context

Memory and personal preferences make two sessions behave differently on the same repository.

## Decision

Agents use no memory outside the repository. The repository holds nothing specific to whoever drives a session. A request for something personal either becomes a rule for everyone or does not happen.

## Consequences

- Any agent or person can pick up the work from the repository alone.
- Harness memory is disabled in the bindings.

## Alternatives rejected

- Per-pilot override files: variance by design.
