# Engineering: git

## Branches

| Branch | Owner | Contents |
|---|---|---|
| `main` | lead | Always green. One squashed commit per task |
| `task/T-NNNN` | the role the task names | The task's work and its report |
| `review/T-NNNN.<round>` | reviewer | Only the review report |

Every role works in its own isolated workspace created from main and commits on
its own branch. Nobody but the lead commits to main.

## Commits on main

Every commit on main follows Conventional Commits 1.0.0
(https://www.conventionalcommits.org/en/v1.0.0/). The lead squashes each task
branch into one commit:

```
<type>(<scope>): <description>

<what changed, two to five lines>

Task: T-NNNN
Spec: <arfc sections>
```

Subject:

- The whole subject line is 72 characters or fewer, imperative, with no
  trailing period.
- `<description>` is the task title in imperative form. It starts with a
  lowercase letter unless its first word is an identifier or a proper name.
- `<scope>` is required and is chosen by the first path in the task's
  `touches` list: under `crates/`, the crate name (`alloy-link`); under
  `.github/`, `ci`; any other top-level directory, its name (`plan`, `agent`,
  `adr`, `spec`, `research`, `product`, `ui`, `xtask`, `tests`); a file at the
  repository root, `repo`.
- A breaking change puts `!` before the colon: `feat(alloy-wire)!: ...`.

Types:

| Type | Use for |
|---|---|
| `revert` | A revert of an earlier commit on main (`agent/core/6-pace.md`) |
| `feat` | New behavior in product code or a new `xtask` capability |
| `fix` | A correction to behavior that contradicts a spec, a test, or an invariant |
| `perf` | A speedup with no change in observable behavior |
| `refactor` | A restructuring with no change in observable behavior or speed |
| `test` | Tests only, including conformance suites |
| `spec` | ARFCs and ADRs |
| `docs` | Non-normative documents: `agent/`, `product/`, `research/`, user docs, READMEs |
| `build` | Cargo manifests, dependencies, toolchain |
| `ci` | CI configuration |
| `chore` | `plan/` bookkeeping, and anything no row above fits |

If a task fits more than one type, use the first matching row of the table.

Body: two to five lines saying what changed, separated from the subject by one
blank line.

Footers, separated from the body by one blank line, one per line, in this order:

- `Task: T-NNNN`, always present on a task squash. A paired build and verify
  squash lists both ids, build first: `Task: T-0042, T-0043`.
- `Spec: <arfc sections>`, present when the task's `spec` field is not empty,
  with its entries copied in order: `Spec: arfc-0005#3.2, arfc-0005#3.3`.
- `BREAKING CHANGE: <what breaks and what users of it must do>`, present
  exactly when the subject carries `!`. Use it for any change to a frozen
  interface, a frozen wire format, a frozen on-disk format, or a stable public
  API. Such a change is pivotal (`agent/core/3-autonomy.md`), so the footer
  also names its ADR.

No other footers. No tool, vendor, or co-author trailers. Commits look the same
whoever or whatever produced them.

The lead's own commits to `plan/` use the same format with type `chore` and
scope `plan`. Their body is optional and their `Task:` footer lists the tasks
they cut, cancel, or change, if any.

Commits on task branches are free-form. They disappear in the squash.

Example:

```
feat(alloy-link): add handshake retry with bounded backoff

The initiator retries a lost handshake message up to the limit in the
ARFC, then reports the peer unreachable.

Task: T-0042, T-0043
Spec: arfc-0003#4.2
```

## Merging

1. Rebase or merge main into the task branch.
2. `cargo xtask ci` passes on the result.
3. Squash onto main.
4. Delete the task branch and its workspace.

A paired verify branch is merged into its build branch first
(`agent/protocol/tasks.md`), and the two land as one commit whose subject comes
from the build task and whose footer names both task ids:
`Task: T-0042, T-0043`.

## Never

- Force-push main, rewrite main's history, or skip hooks.
- Commit generated build output, secrets, or files over 1 MiB without an ADR.
- Commit anything listed in `.gitignore` by forcing it.
