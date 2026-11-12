# Harness bindings: Claude Code

This file maps the vendor neutral terms used in `agent/` to Claude Code. It holds
bindings only. Behavior lives in `agent/`. If the two disagree, `agent/` wins and
this file has a bug.

## Tiers

| Tier (see `agent/core/5-delegation.md`) | Model |
|---|---|
| build | `opus` (the main session, which is the lead, and five roles) |
| scan | `sonnet` |
| deep | `fable` |

Run the lead session on `opus`. Role tiers are set in the frontmatter of
`.claude/agents/*.md`. The only per dispatch override is the deep tier exception:
pass `model: fable` to the `Agent` call for a task whose file says
`executor: deep`.

## Terms

| Neutral term | Claude Code |
|---|---|
| dispatch a role | `Agent` tool, `subagent_type` set to the role name, `prompt` set to the dispatch message verbatim and nothing else |
| parallel dispatch | several `Agent` calls in one turn, run in the background |
| isolated workspace | `isolation: worktree` (set in the role's frontmatter) |
| structured question to the pilot | `AskUserQuestion` |
| instruction entry file | `AGENTS.md`, read natively when no `CLAUDE.md` exists (v2.1.277 and later) |

## Rules specific to this harness

- Only the six roles in `.claude/agents/` are dispatched. Built-in agent types are
  not used.
- Auto memory is off in `.claude/settings.json`. Do not use memory tools, memory
  directories, or anything under `~/.claude/` for project knowledge.
- Never create `CLAUDE.md`, `CLAUDE.local.md`, or `.claude/settings.local.json`
  for this repository.
- The task widget may mirror the board. `plan/STATE.md` is the truth.
- A session on a harness version that does not read `AGENTS.md` natively still
  works: the start prompt in `AGENTS.md` names the files to read.
