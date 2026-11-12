# ADR 0004: AGENTS.md is the only instruction entry file

- Status: accepted
- Subject: process
- Specs: none

## Context

Several agent tools read different instruction files. Duplicates drift.

## Decision

`AGENTS.md` is the single entry and only points to `agent/ROOT.md`. `CLAUDE.md` and other vendor instruction files are never added.

## Consequences

- Tools that do not read `AGENTS.md` are started with the prompt in `AGENTS.md`.
- `.gitignore` and `cargo xtask check-repo` reject vendor instruction files.

## Alternatives rejected

- One file per vendor: guaranteed drift.
