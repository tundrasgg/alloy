---
task: T-NNNN
role: <role>
round: <n>
status: <done | blocked | failed>
branch: <branch>
---

## Summary

<Five lines at most. What was done, stated as fact. The whole report targets
60 lines or fewer, a reviewer's 100 or fewer. A verifier's clause-to-test
table, one line per clause, does not count toward the target.>

## Changes

- `<path>`: <one line: what the file now holds, not how you got there>

## Evidence

```
$ <command>
<only the lines that show the result: the summary line of a run, the failing line>
```

## Decisions

- <One line each: a class N decision and its reason. "none" if none.>

## Issues

- <`spec-issue` | `test-issue` | `process-issue` | `follow-up`>: <one line, with a path>

## Findings

<Reviewer only. The table from `agent/protocol/messages.md`. Omit otherwise.>

## Question

<Only when status is `blocked`. One question, two to four options, your
recommendation first.>
