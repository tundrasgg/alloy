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
4. **Review.** Lead dispatches the reviewer on `task/T-A`. Board: `review`. A
   task that Review below exempts skips this step and step 5.
5. **Fix.** `block` findings go back to the developer once. `fix` findings go
   back in the same round if there are any `block` findings. Every other `fix`
   and `note` finding is folded (Folding findings below).
6. **Merge.** Lead squashes `task/T-A` onto main as one Conventional Commits
   commit whose footer names both tasks, `Task: T-A, T-B`, and marks both tasks
   `done`. See `agent/eng/git.md`.

Research, product, spec, and process tasks skip step 3, and take steps 4 and 5
only when Review below requires a review.

## Review

One review by the reviewer role is required before merge for:

- a `build` or `interface` task,
- a `spec` or `process` task that changes behavior in a security-relevant area:
  cryptography, authentication, authorization, tenant isolation, or a wire or
  on-disk format.

A `verify` task is reviewed together with its paired build task, on the build
task's branch (step 4).

No review for:

- a `spec` clarification, which changes no behavior
  (`agent/protocol/spec-change.md`),
- a `research` task,
- a `build` task under 100 changed lines confined to `xtask/`, `.github/`, or
  `fuzz/`, once the lead has run its acceptance commands on its branch and they
  gave the expected results.

Every other task merges without review.

## Folding findings

A `fix` or `note` finding that is not sent back in a `block` fix round is
recorded on the board: the report path, the finding id, and the files its
`where` names. When the lead cuts the next task whose `touches` covers one of
those files, it puts the finding in that task's `Scope`, adds the report to its
`reads`, and removes the finding from the board. The lead cuts a task just for
recorded findings only when no task that touches the same files is coming.

## Readiness

A task is ready to dispatch only when:

- every task in `depends` is `done`,
- every path in `reads` exists on main,
- the acceptance section contains runnable commands,
- its `touches` set is disjoint from every `active` task, leaving out the shared
  index files and the shared workspace files below.

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

## Shared workspace files

A task that adds a crate or a dependency lists these paths in `touches` (D-0006).
They are shared, and only these parts of them:

- the `members` list of `[workspace]` in the root `Cargo.toml`,
- the `allow` list of `[bans]` in the root `deny.toml`,
- `Cargo.lock`.

In the two lists, a task may add entries. It may not edit, delete, or reorder an
existing entry. Each list keeps one entry per line in ascending byte order, by
path in `members` and by `crate` in `allow`, and a new entry goes at its sorted
place. A task that finds a list on one line may break it into one entry per
line. A task changes `Cargo.lock` only by running cargo after its own manifest
changes. It never edits the file by hand and never runs `cargo update` or
`cargo generate-lockfile`, which move existing versions.

The disjointness check in the readiness rules leaves these paths out, so any
number of tasks that each add crates or dependencies can be `active` at once. A
task whose scope removes a member, removes an `allow` entry, changes anything
else in `Cargo.toml` or `deny.toml` (a workspace lint, a license), or moves a
version already in `Cargo.lock` holds that file exclusively: for that task the
path counts in the disjointness check like any other.

A merge conflict in a shared workspace file is resolved by the lead, at merge,
while it brings main into the task branch (`agent/eng/git.md`):

1. `Cargo.toml` and `deny.toml`: keep both sides' added entries, in ascending
   byte order, and change nothing else.
2. `Cargo.lock`: never merge it by hand. Take main's version
   (`git checkout main -- Cargo.lock`), then run `cargo update --workspace`,
   which updates only workspace packages and packages not yet in the lockfile
   (`cargo help update`, option `--workspace`).
3. Run `cargo xtask ci` on the result.

A conflict that is not two sets of added entries means a task broke the rules
above, and a `cargo xtask ci` failure after step 2 means the task's work does not
build against main's locked versions. In either case the lead does not resolve
it and dispatches that task's role again with the next round.

## Sizing

Target 400 changed lines or fewer and one crate. A `build` or `verify` task
against an interface already merged on main targets 2,000 changed lines or fewer,
still in one crate and on one concern. If a task cannot be described in a goal of
three sentences, split it.
