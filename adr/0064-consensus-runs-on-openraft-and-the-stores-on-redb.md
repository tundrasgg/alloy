# ADR 0064: Consensus runs on openraft, and the stores on redb

- Status: accepted
- Subject: control plane
- Specs: arfc-0006, arfc-0018

## Context

ARFC 0006 section 5 left Raft through `openraft` and `native` stores on `redb` pending research, and M11 rung 2 needs a working one-voter control plane. R-0004 compared the candidates: `openraft` is the only maintained Raft crate that passes `agent/eng/deps.md` and has a pluggable runtime, and `redb` has no dependencies and a documented commit and recovery design. `redb` does its file access synchronously, through its own file code or a synchronous `StorageBackend` trait (`redb` 4.5.0 `src/db.rs:74`), while every `alloy-rt` seam is asynchronous.

## Decision

- Raft is `openraft`, pinned `=0.10.0` with `default-features = false`. Only `alloy-state` depends on it. Its `AsyncRuntime` is implemented on `alloy-rt`, so every timer, task, and random draw goes through a `Runtime`.
- Each consensus group has one `log-store` instance and one `state-store` instance (ARFC 0006 section 5). The traits, their rules, and the export format are Alloy's own; `openraft` is adapted to them inside `alloy-state`.
- `native` for both seams is `redb`, pinned `=4.5.0`. The log commits with two-phase commit and immediate durability, the state store with immediate durability and the default commit (R-0004, Recommendation).
- Exception to ADR 0008: the `native` stores open and access their own two files through `redb`'s file code, not through `disk-io`, and run only on `os` runtimes. Simulated hosts bind `mem`. Nothing else in Alloy gains this exception.
- Store bindings are node-local configuration, never control plane state.

## Consequences

- One `alloyd` runs both groups of a one-region cluster on one voter with no external service (ADR 0019, I-16).
- Determinism of `openraft` on the simulator is unproven (R-0004, Unknown): the first task that runs a group on `alloy-sim` gates on a seed replay test (I-20).
- `native` crash behavior is `redb`'s and is not exercised by simulated power loss; its conformance suite runs on the CI host. A store over `disk-io` that the simulator can crash needs a new ADR that supersedes this exception.
- Each group commits its own audit records with its changes (ARFC 0018 clause 3.3.3), so each group keeps its own audit chains.
- `openraft` is pre-1.0. A minor upgrade is a dependency task that reruns both store suites and the replay test.

## Alternatives rejected

- `raft` (raft-rs) 0.7.0: no release since 2023 and a hard `protobuf` 2 dependency under RUSTSEC-2024-0437.
- Own Raft: consensus safety with no external review, far beyond one task.
- `fjall` 3.1.13: 47 transitive crates against none, and no sync by default. Kept as the fallback behind `log-store`.
- `heed` (LMDB): the OpenLDAP license is not on the allowlist.
- A `redb` backend over `disk-io`: a synchronous trait over asynchronous seams would block runtime threads under `os`, and the simulator does not use `native`.
