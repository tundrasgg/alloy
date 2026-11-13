# Agent root

Every agent that works on Alloy starts here. Humans follow the same rules.

## Session start procedure

Do these in order, every session, with no exceptions.

1. Read this file.
2. Read all six files in `agent/core/`. They are short and they bind every role.
3. Read `agent/protocol/messages.md`. The lead reads all of it. Every other role
   reads its sections Report (role to lead) and Issues raised in reports, plus
   Later rounds when its round is above 1, plus Findings (reviewer) when it is
   the reviewer.
4. Read your role file in `agent/roles/`.
5. Lead: if your harness has a bindings file, read it. Claude Code:
   `.claude/harness.md`. Other roles skip it: it maps dispatch and tiers, which
   only the lead uses.
6. Lead: read `plan/STATE.md`, then `agent/protocol/tasks.md`.
   Every other role: read the task file named in your dispatch message, then each
   entry in its `reads` list, only the named section where an entry names one
   (`agent/protocol/tasks.md`, Reads).
7. Begin work. Load anything else only when the task calls for it, and then only
   the section you need.

You have no memory of earlier sessions and you must not try to have one. The
repository is the only state. If something is not in the repository, you do not
know it.

## Roles

| Role | Tier | Does | Never does |
|---|---|---|---|
| `lead` | build | Plans, cuts tasks, dispatches, merges, keeps `plan/STATE.md` | Writes product code, reviews diffs |
| `architect` | build | Writes ARFCs and ADRs | Writes product code |
| `developer` | build | Implements one task in one crate | Edits specs or conformance tests |
| `verifier` | build | Writes conformance tests from the spec | Reads the implementation |
| `reviewer` | build | Reviews and attacks a task branch | Fixes what it finds |
| `researcher` | scan | Answers one question with sources | Decides |
| `product` | build | Owns user-facing surfaces and user docs | Writes product code |

A third tier, deep, is reserved for rare, extremely high stakes tasks. See
`agent/core/5-delegation.md`.

## Map

| Path | What | Who writes |
|---|---|---|
| `agent/core/` | Behavior that binds everyone | architect, by `process` task |
| `agent/protocol/` | Message, task, decision, and spec change formats | architect, by `process` task |
| `agent/roles/` | One file per role | architect, by `process` task |
| `agent/eng/` | Engineering rules: Rust, dependencies, testing, crypto, git | architect, by `process` task |
| `agent/domain/` | Glossary and global invariants | architect |
| `agent/templates/` | File templates. Copy them exactly | architect, by `process` task |
| `spec/` | ARFCs, the normative design | architect |
| `adr/` | One decision per file | architect |
| `product/` | Principles and briefs | product |
| `research/` | Research notes | researcher |
| `plan/STATE.md` | The board. Single source of live status | lead only |
| `plan/tasks/` | Task files, immutable after dispatch | lead only |
| `plan/reports/` | One report per task, role, and round, removed once its task is done | the reporting role |
| `plan/decisions.md` | One line per notable decision | lead only |
| `crates/`, `ui/`, `xtask/` | Product code | developer |
| `tests/conformance/` | Conformance suites | verifier only |

## Routing: what to read for what

| If the task involves | Read |
|---|---|
| Any Rust code | `agent/eng/rust.md`, `agent/eng/testing.md` |
| A new dependency | `agent/eng/deps.md` |
| Keys, handshakes, AEAD, tokens, randomness | `agent/eng/crypto.md` |
| Branches, commits, merges | `agent/eng/git.md` |
| A term you do not know | `agent/domain/glossary.md` |
| Review of any kind | `agent/domain/invariants.md` |
| A subsystem | the sections you need of its ARFC, found through `spec/README.md` |
| Why something is the way it is | `adr/README.md` |
| Crate boundaries | `plan/workspace.md` |

## The short version

- The spec is law. Code follows it or a spec change is filed.
- The lead delegates. Subagents do one task and report with evidence.
- Agents talk through files in fixed formats, never through free prose.
- Nothing personal to whoever is driving the session enters the repository.
- Decide and move. Ask the pilot only when a decision is pivotal.
