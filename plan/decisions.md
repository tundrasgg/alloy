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
