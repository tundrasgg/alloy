---
task: T-NNNN
role: <role>
round: <n>
status: <done | blocked | failed>
branch: <branch>
---

## Summary

<Five lines at most. What was done, stated as fact.>

## Changes

- `<path>`: <one line>

## Evidence

```
$ <command>
<relevant output, trimmed>
```

## Decisions

- <Class N decisions made, one line each, with the reason. "none" if none.>

## Issues

- <`spec-issue` | `test-issue` | `process-issue` | `follow-up`>: <one line, with a path>

## Findings

<Reviewer only. The table from `agent/protocol/messages.md`. Omit otherwise.>

## Question

<Only when status is `blocked`. One question, two to four options, your
recommendation first.>
