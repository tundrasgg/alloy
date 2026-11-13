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
the same role is dispatched again for the same task (see Later rounds).

The lead commits the task file to main before dispatching, so that an isolated
workspace created from main contains it.

## Later rounds

A dispatch with `round` greater than 1 carries the same five fields and nothing
more. What the role starts from and what it reads follow from the task id and the
round alone.

Before dispatching a later round, the lead:

1. Commits to main every reviewer report for the task that is not there yet. A
   `review/T-NNNN.<round>` branch holds only its report, so main is where the
   next round finds it.
2. Makes sure no workspace holds `task/T-NNNN` checked out, by removing the
   workspace of the earlier round. Its commits are on the branch, so nothing is
   lost, and the new workspace can check the branch out.

The role, in a round greater than 1, reads its task file and the `reads` list as
in round 1 (`agent/ROOT.md`), and then, before changing anything:

1. **Starts from its branch.** It continues on `task/T-NNNN`, the branch the
   earlier rounds committed to. If its workspace was created from main, it checks
   out `task/T-NNNN` before any other step. Then it merges main into
   `task/T-NNNN`, so the workspace holds both the earlier rounds' work and the
   reports the lead committed to main. All its commits in this round go on
   `task/T-NNNN`, and its report names that branch.
2. **Reads the earlier reports.** It reads every report for the task written in
   an earlier dispatch, of every role, reviewer reports included: every file
   `plan/reports/T-NNNN.*.md` in its workspace. These are the inputs of the
   round. Its report says, by finding id in its `Changes` section, how each
   `block` and `fix` finding it was sent was resolved.

A reviewer in a round greater than 1 is the exception to step 1: it never commits
to `task/T-NNNN`. It reviews the current tip of `task/T-NNNN`, reads the reports
of step 2 from main and from that branch, and commits its report on a new
`review/T-NNNN.<round>` branch created from main.

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

A report is read once, by the lead, and leaves the tree when its task is done
(`agent/protocol/tasks.md`, Report lifetime). It holds only what the lead acts on:

- It targets 60 lines or fewer, a reviewer's 100 or fewer. A verifier's
  clause-to-test table, one line per clause, does not count toward the target.
- Evidence quotes the command and only the lines that show its result: the
  summary line of a test run, the failing line, the exit status. Never a whole
  log.
- Changes lists files, one line each, saying what the file now holds. No
  narration of how the work went.
- Each decision and each issue is one line.

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
