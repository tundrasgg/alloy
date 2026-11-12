# ADR 0003: Alloy is built by coordinated agents under rules in the repository

- Status: accepted
- Subject: process
- Specs: none

## Context

Most work is done by AI agents. Their behavior has to be identical across sessions and vendors.

## Decision

Everything that shapes agent behavior lives in `agent/`. Roles, protocol, and engineering rules are files, changed by `process` tasks. Humans follow the same files.

## Consequences

- A new contributor of either kind needs only the repository.
- Vendor directories hold bindings and nothing else.

## Alternatives rejected

- Behavior in vendor-specific prompt files: it diverges per tool and per person.
