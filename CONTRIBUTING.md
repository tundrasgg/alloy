# Contributing

Humans and agents follow the same process. There is one rulebook: `agent/`.

1. Read `agent/ROOT.md` and the six files in `agent/core/`.
2. Find work on the board in `plan/STATE.md`, or propose a task (see
   `agent/protocol/tasks.md`).
3. Work on a `task/T-NNNN` branch. One task, one crate, one squashed commit on main.
4. `cargo xtask ci` must pass. Evidence goes in a report file
   (`agent/templates/report.md`).
5. Behavior that differs from a spec is a bug in the code or a change request for
   the spec (`agent/protocol/spec-change.md`), never a silent divergence.

Contributions are licensed MIT OR Apache-2.0. Dependencies must be compatible with
that dual license (`agent/eng/deps.md`).

To change how agents behave, change the files in `agent/` through the same
process. Those files apply to everyone equally.
