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

The lead squashes each task branch into one commit:

```
T-NNNN: <task title>

<what changed, two to five lines>

Spec: <arfc sections>
```

- Subject is 72 characters or fewer, imperative, with no trailing period.
- No tool, vendor, or co-author trailers. Commits look the same whoever or
  whatever produced them.
- Commits on task branches are free-form. They disappear in the squash.

## Merging

1. Rebase or merge main into the task branch.
2. `cargo xtask ci` passes on the result.
3. Squash onto main.
4. Delete the task branch and its workspace.

A paired verify branch is merged into its build branch first
(`agent/protocol/tasks.md`), and the two land as one commit that names both task
ids in the subject: `T-0042, T-0043: <title>`.

## Never

- Force-push main, rewrite main's history, or skip hooks.
- Commit generated build output, secrets, or files over 1 MiB without an ADR.
- Commit anything listed in `.gitignore` by forcing it.
