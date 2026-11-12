# Decision log

Class N decisions, one line each, written only by the lead. Format and rules:
`agent/protocol/decisions.md`. Decisions that constrain design are ADRs in `adr/`.

- D-0001 (none) The spec series is named ARFC, for Alloy RFC. Why: one short, searchable name for normative documents.
- D-0002 (none) The quality gate is `cargo xtask ci` and the repository policy lint is `cargo xtask check-repo`. Why: one entry point that works the same on every machine and CI system.
- D-0003 (none) Crate boundaries start as listed in `plan/workspace.md`. Why: boundaries follow the ARFCs so that one task maps to one crate.
- D-0004 (T-0015) The canonical repository is the public GitHub repository `tundrasgg/alloy` and CI runs on GitHub-hosted runners. Why: pilot answer to a pivotal question on publishing and accounts; ADR 0037 records it.
