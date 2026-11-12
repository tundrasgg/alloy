# ADR 0037: GitHub hosts the source and CI runs on GitHub-hosted runners

- Status: accepted
- Subject: project
- Specs: none

## Context

Alloy needs one canonical repository and one place where the quality gate runs.
Both involve outside accounts, so the choice is pivotal and was the pilot's to make.
The gate itself is already fixed: `cargo xtask ci` (D-0002). The repository is
public and accepts pull requests from forks, so CI runs code nobody has reviewed yet.

## Decision

- The canonical repository is the public GitHub repository `tundrasgg/alloy`. Its default branch is `main`.
- CI is GitHub Actions on GitHub-hosted runners. No self-hosted or third-party runners are used.
- CI runs exactly `cargo xtask ci` and nothing else. That job is the only required status check on `main`; no other automated check gates a merge.

## Consequences

- Any check the project wants enforced goes into `cargo xtask ci`, never into a separate workflow job, so CI and local runs stay identical.
- Workflow files only check out the source, set up the toolchain and caches, and call `cargo xtask ci`.
- Fork pull requests run on ephemeral GitHub machines, never on project hardware.
- CI costs nothing while the repository is public. Making it private reopens this decision.
- CI speed is bounded by standard GitHub-hosted runner sizes; caching is the lever.

## Alternatives rejected

- Third-party hosted runners such as Blacksmith: faster, but another vendor account; the pilot rejected them.
- Self-hosted runners: operational burden, and unsafe for a public repository that runs fork pull requests.
- Extra CI jobs beside `cargo xtask ci`: they let CI and local runs diverge.
