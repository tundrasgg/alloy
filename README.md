# Alloy

The cloud is your computer.

Alloy is an open source private cloud. One command starts a cluster, one token
joins a machine to it, and the result behaves like a public cloud that you own:
instances, containers, private networks, public addresses, block volumes, object
storage, and a container registry.

- **Runs anywhere.** One board, one rack, or many sites. A cluster needs a single
  address that its machines can reach.
- **Brings its own network.** Machines form an encrypted overlay with direct
  connections where the network allows and relayed connections where it does not.
- **Multi-tenant.** Organizations and projects are isolated from each other in
  compute, network, and storage.
- **Familiar APIs.** A native API, plus EC2-compatible and S3-compatible surfaces
  and an OCI registry.

```
alloy init --advertise 203.0.113.7     # first machine, prints a join token
alloy join alyt_...                    # every other machine
```

## Status

Design stage. The specifications in `spec/` and the plan in `plan/` are the
current product. Code lands milestone by milestone, see `plan/milestones.md`.

## Repository layout

| Path | Contents |
|---|---|
| `spec/` | ARFCs (Alloy RFCs), the normative design, one per subsystem |
| `adr/` | Architecture decision records, one decision per file |
| `plan/` | Milestones, the task board (`STATE.md`), task files, reports |
| `agent/` | How work is done here: behavior, protocol, roles, engineering rules |
| `product/` | Product principles and briefs for user-facing surfaces |
| `research/` | Research notes with sources |
| `crates/` | Rust workspace members (created by the first tasks) |

## How work happens

Alloy is built by coordinated agents and by humans following the same rules.
Everything a contributor needs is in this repository: `agent/ROOT.md` explains the
process, `plan/STATE.md` says what is happening now. See `CONTRIBUTING.md`.

## License

Licensed under either of Apache License, Version 2.0 (`LICENSE-APACHE`) or MIT
license (`LICENSE-MIT`) at your option. Contributions are accepted under the same
dual license.
