# Decision log

Class N decisions, one line each, written only by the lead. Format and rules:
`agent/protocol/decisions.md`. Decisions that constrain design are ADRs in `adr/`.

- D-0001 (none) The spec series is named ARFC, for Alloy RFC. Why: one short, searchable name for normative documents.
- D-0002 (none) The quality gate is `cargo xtask ci` and the repository policy lint is `cargo xtask check-repo`. Why: one entry point that works the same on every machine and CI system.
- D-0003 (none) Crate boundaries start as listed in `plan/workspace.md`. Why: boundaries follow the ARFCs so that one task maps to one crate.
- D-0004 (T-0015) The canonical repository is the public GitHub repository `tundrasgg/alloy` and CI runs on GitHub-hosted runners. Why: pilot answer to a pivotal question on publishing and accounts; ADR 0037 records it.
- D-0005 (T-0017) Commits on `main` follow Conventional Commits 1.0.0, with task ids in a `Task:` footer. Why: pilot direction; one machine-readable history format for changelogs and tooling.
- D-0006 (T-0001) A task that adds a crate lists `Cargo.toml`, `Cargo.lock`, and `deny.toml` in its `touches`. Why: `deny.toml` allowlists workspace members too, and the gate runs with `--locked`.
- D-0007 (T-0005) No frequency-weighted NAT tests until a source separates home routers by RFC 4787 mapping and filtering; prevalence stays unknown. Why: R-0003 found no such measurement.
- D-0008 (T-0029) Install scripts are served from GitHub Releases of `tundrasgg/alloy`; there is no separate download host. Why: pilot answer to a pivotal question; no new domain to own, and the URL lives as long as the repository. ADR 0044 records it.
- D-0009 (T-0001) Review `fix` findings on T-0001 are folded into T-0002, which was not yet dispatched and owns `xtask/`. Why: same crate and concern; a separate task would have to wait on T-0002 anyway.
- D-0010 (T-0026) When a review's `fix` findings concern an ADR that is not yet on main, the lead sends them back in a second round instead of merging and cutting a follow-up. Why: an accepted ADR cannot be edited, so merging a known-wrong ADR costs a superseding ADR later.
- D-0011 (T-0036) The `check-repo` links rule does not check `plan/`, and accepts `path:N-M` line ranges. Why: task files and reports are immutable and cite files on unmerged branches, so violations there could never be fixed and main would stay red.
- D-0012 (T-0002) The 60-character task title limit in `agent/templates/task.md` is not enforced by `check-repo`. Why: dispatched task files T-0026 and T-0027 exceed it and cannot be edited.
- D-0013 (T-0040) T-0023 merges with its re-review's block finding (unbounded lineage state, I-21) open, and T-0040 fixes it before any M2 build task cites ARFC 0007 section 3.9. Why: its fix round and re-review were spent, its ruling against ARFC 0020 holds, and T-0022 needs ARFC 0007 settled on main.
- D-0014 (T-0040) T-0040 needs no new ADR: it makes the guarantees ADR 0041 already states hold. Spec tasks that may change behavior include `adr/` in `touches` from now on. Why: `agent/protocol/spec-change.md` asks for an ADR on behavior changes, and T-0040's `touches` wrongly excluded `adr/`.
- D-0015 (T-0043) T-0021 and T-0031 merge with the re-review's two block findings open, and T-0043 fixes them; T-0039 is cancelled into T-0043. Why: the fix round and re-review were spent, the merged text is strictly better than main (probe amplification removed), and the return path gap already existed on main.
- D-0016 (T-0018) On main, the path lock judges each commit by the task types in its `Task:` footer: a commit that changes locked paths fails unless one of its tasks is `verify`. The lead runs `cargo xtask ci --base main --branch task/T-NNNN` when integrating. Why: on a push to main the branch name is `main`, and on a pull request the author chooses it, so neither can be trusted to name the task (T-0037 review notes F1, F2).
- D-0017 (T-0046) The `direct-io` ban moves to Clippy `disallowed-methods` and `disallowed-types`, with `alloy-rt` and `alloy-sim` opting out; text patterns remain only where Clippy cannot see a call. Why: line-based matching keeps missing grouped imports, renamed imports, and macro expansions (T-0036 and T-0042 reviews).
- D-0018 (T-0006) `alloy-seam` is listed above `alloy-rt` in `plan/workspace.md`, and `alloy-rt` depends on `alloy-seam`. Why: `alloy-rt` declares its seam markers with `alloy-seam` types (ARFC 0001 section 2.1).
- D-0019 (T-0018) `check-commits` applies to commits after `78d3175`; earlier commits on main are not checked. T-0018's acceptance range is `78d3175..main`. Why: three lead commits up to `78d3175` exceed the 72-character subject limit, and main's history is never rewritten.
