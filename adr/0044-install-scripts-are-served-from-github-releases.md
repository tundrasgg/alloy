# ADR 0044: Install scripts are served from GitHub Releases

- Status: accepted
- Subject: project
- Specs: none

## Context

PB-0001 starts every install with `curl ... | sh` (or `irm ... | iex`) against a
download host it leaves as `<download-host>`. That URL is public, printed in docs and
errors, and costly to change, so the choice is pivotal and was the pilot's to make.
The source already lives at `tundrasgg/alloy` on GitHub (ADR 0037). The installer
verifies each release's signature (ARFC 0017 §3.8), so integrity does not rest on the host.

## Decision

- `install.sh` and `install.ps1` are release assets of the GitHub repository `tundrasgg/alloy`.
- The latest release's scripts are fetched from
  `https://github.com/tundrasgg/alloy/releases/latest/download/install.sh` and
  `https://github.com/tundrasgg/alloy/releases/latest/download/install.ps1`.
- A pinned release's scripts are fetched from
  `https://github.com/tundrasgg/alloy/releases/download/<tag>/install.sh` and `.../install.ps1`,
  where `<tag>` is that release's tag.
- No other download host serves Alloy's install scripts.

## Consequences

- The release process must attach both `install.sh` and `install.ps1` to every release.
- PB-0001's `https://<download-host>/install.sh` and `.../install.ps1` become the URLs above.
- No domain, certificate, or web server needs to be owned or run for installs.
- The URLs stay valid as long as the repository keeps its name; renaming or moving it reopens this decision.

## Alternatives rejected

- A subdomain of an existing domain: another name to own and serve, tied to that domain's fate.
- A dedicated Alloy domain: a new domain to register, renew, and run.
