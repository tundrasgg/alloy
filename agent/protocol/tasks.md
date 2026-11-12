# Protocol: tasks

## Task files

A task is one file, `plan/tasks/T-NNNN.md`, copied from
`agent/templates/task.md`. Only the lead creates task files. A task file is
immutable once dispatched. If a task turns out wrong, the lead cancels it on the
board and cuts a new one.

A task file carries no status. Status lives only on the board.

## Types

| Type | Role | Output |
|---|---|---|
| `research` | researcher | `research/R-NNNN.md` |
| `product` | product | `product/briefs/PB-NNNN.md` or user docs |
| `spec` | architect | ARFC and ADR changes |
| `interface` | developer | Trait and type definitions, no behavior |
| `build` | developer | Implementation in one crate |
| `verify` | verifier | Conformance tests for one spec section |
| `process` | architect | Changes under `agent/` |

Every `build` task that implements spec behavior has a paired `verify` task cut
from the same spec section. They run in parallel against an interface that an
earlier `interface` task froze. Tooling tasks with no spec behavior have no pair
and carry their own tests.

## The board

`plan/STATE.md` is written only by the lead. It holds the current milestone, the
next free ids, and one row per open task.

Statuses: `queued` (waiting on the tasks it depends on), `ready`, `active`,
`review`, `blocked` (reported blocked, needs the lead), `done`, `failed`,
`cancelled`.
Rows for `done` and `cancelled` tasks are removed at each milestone exit. Git
keeps the history.

## Lifecycle of a build task

1. **Cut.** Lead writes `T-A` (build) and `T-B` (verify), commits, sets both
   `ready`.
2. **Dispatch.** Both go out in one turn. Board: `active`.
3. **Integrate.** When both report `done`, the lead merges `task/T-B` into
   `task/T-A` and runs `cargo xtask ci` on the result.
   - Tests fail: dispatch the developer again with `round: 2`. Its branch now
     contains the conformance tests, and running them is how it learns what
     failed.
   - The developer claims a test is wrong: it reports a `test-issue`. The lead
     dispatches the architect to rule on it against the spec.
4. **Review.** Lead dispatches the reviewer on `task/T-A`. Board: `review`.
5. **Fix.** `block` findings go back to the developer once. `fix` findings go
   back in the same round if there are any `block` findings, otherwise they become
   a follow-up task. `note` findings are recorded and not acted on.
6. **Merge.** Lead squashes `task/T-A` onto main as one Conventional Commits
   commit whose footer names both tasks, `Task: T-A, T-B`, and marks both tasks
   `done`. See `agent/eng/git.md`.

Research, product, spec, and process tasks skip steps 3 to 5. A spec task that
changes behavior gets one review by the reviewer role before merge.

## Readiness

A task is ready to dispatch only when:

- every task in `depends` is `done`,
- every path in `reads` exists on main,
- the acceptance section contains runnable commands,
- its `touches` set is disjoint from every `active` task, leaving out the shared
  index files below.

## Shared index files

These paths are shared and append-only:

- `adr/README.md`
- `spec/README.md`
- `product/briefs/README.md`
- `research/README.md`

A task that lists one of them in `touches` may add rows to its tables. It may not
edit, delete, or reorder an existing row, and it may not change any text outside
the tables. A new row goes in the table where its id belongs, in ascending id
order.

The disjointness check in the readiness rules leaves these paths out, so any
number of tasks that each add rows to the same index can be `active` at once. A
task whose scope edits an existing row, for example an ARFC status in
`spec/README.md`, holds that file exclusively: for that task the path counts in
the disjointness check like any other.

A merge conflict in a shared index is resolved by the lead, at merge, while it
brings main into the task branch (`agent/eng/git.md`). The lead keeps both sides'
new rows, in ascending id order, and changes nothing else. A conflict that is not
two sets of added rows means a task broke the append-only rule: the lead does not
resolve it and dispatches that task's role again with the next round.

## Sizing

Target 400 changed lines or fewer and one crate. If a task cannot be described in
a goal of three sentences, split it.
