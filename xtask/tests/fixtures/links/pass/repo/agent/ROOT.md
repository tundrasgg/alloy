# Root

Read [the Rust rules](eng/rust.md), the [spec index](../spec/README.md#arfcs),
and [the root](/agent/ROOT.md). External links such as
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) and
[mail](mailto:someone@example.org) are not checked, and neither is a
[same-file anchor](#root).

Backticked paths resolve: `agent/eng/rust.md`, `agent/eng/rust.md:3`,
`agent/eng/rust.md:1-3`, `spec/`, `spec/README.md#arfcs`, `plan/STATE.md`.
Placeholders and future code paths are not checked:
`plan/tasks/T-NNNN.md`, `adr/0001-<slug>.md`, `crates/*/src`,
`tests/conformance/rt/`, `cargo xtask check-repo`.

[eng]: ./eng/rust.md

```
A fenced block is not checked: [broken](nowhere.md) and `agent/gone.md`.
```
