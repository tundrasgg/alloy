# Protocol: messages

Agents communicate through files in fixed formats. There are exactly two message
types, and both are pointers to files. Agents never message each other directly:
every exchange goes through the lead and through the repository.

## Dispatch (lead to role)

The entire prompt of a dispatched role is:

```
ALLOY DISPATCH v1
role: <role>
task: T-NNNN
round: <n>
file: plan/tasks/T-NNNN.md
```

Nothing may be added. Context that is not in the task file or reachable from its
`reads` list does not exist for the role. `round` starts at 1 and increases when
the same role is dispatched again for the same task.

The lead commits the task file to main before dispatching, so that an isolated
workspace created from main contains it.

## Report (role to lead)

The role first writes its report file at
`plan/reports/T-NNNN.<role>.<round>.md` from `agent/templates/report.md`, commits
it on its branch (`agent/eng/git.md`), and then replies with exactly:

```
ALLOY REPORT v1
task: T-NNNN
role: <role>
round: <n>
status: done | blocked | failed
branch: <branch>
report: plan/reports/T-NNNN.<role>.<round>.md
```

- `done`: every acceptance command passed and the evidence is in the report.
- `blocked`: the role cannot proceed. The report's `question` section holds
  exactly one question with options.
- `failed`: the role tried and could not meet acceptance. The report says what was
  tried and what the output was.

## Findings (reviewer)

A reviewer's report lists findings in this table and nowhere else:

| Field | Content |
|---|---|
| id | `F1`, `F2`, ... |
| severity | `block` (violates a spec or an invariant, or is unsafe), `fix` (wrong but not dangerous), `note` (optional improvement) |
| where | `path:line` |
| claim | What is wrong, one sentence |
| evidence | Spec clause, invariant id, failing command, or reproducing seed |
| change | The required outcome, not the patch |

A finding without evidence is dropped by the lead.

## Issues raised in reports

Any role may list issues in its report's `issues` section using one of these
tags. The lead turns each into a task or a decision.

- `spec-issue`: a spec is wrong, silent, or contradicts another
- `test-issue`: a conformance test contradicts the spec
- `process-issue`: a rule in `agent/` cost time without protecting anything
- `follow-up`: work that is out of scope for this task but needed
