# Alloy

Alloy is an open source private cloud: compute, networking, block and object
storage, and a container registry, on any hardware from one board to many racks,
joined by its own encrypted overlay network.

## Agents: start here

Read `agent/ROOT.md` now, before any other action, and follow it. It is the single
source of behavior for every agent that works in this repository. This file only
points there.

## Facts

- Language: Rust (Cargo workspace at the repository root). Console UI: TypeScript.
- License: MIT OR Apache-2.0.
- Specifications: `spec/` (the ARFC series). Decisions: `adr/`. Plan and live state: `plan/`.
- The single quality gate is `cargo xtask ci`. Nothing merges unless it passes.
- This repository has no `CLAUDE.md` and must never gain one. Vendor specific
  configuration lives only in that vendor's directory (for example `.claude/`) and
  holds bindings, never behavior.

## Starting a session

The whole prompt for a new lead session is:

    Read AGENTS.md, then agent/ROOT.md, and act as the lead. Continue from plan/STATE.md.
