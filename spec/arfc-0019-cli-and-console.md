# ARFC 0019: CLI and console

- Status: draft
- Depends on: 0008
- Crates: alloy (binary), ui/console
- Decisions: 0002

## 1. Purpose

The two surfaces people use directly. Both are plain clients of the native API.
This ARFC fixes their structure. Their wording and flows come from product briefs,
starting with PB-0001 (task T-0013).

## 2. Model

The CLI is one binary, `alloy`, for Linux, macOS, and Windows. It holds
**contexts**: a cluster endpoint, a pinned CA fingerprint or public name, a
credential, and a default org and project.

The console is a web application served by control nodes at the API endpoint. It
is written in TypeScript and built into static files embedded in `alloyd`.

## 3. Behavior

### 3.1 CLI grammar

3.1.1 Commands are `alloy <noun> <verb> [name] [flags]`: `alloy instance create
web --image ubuntu-24.04 --shape small`. Nouns are the resource kinds in their
glossary spelling. Verbs are `create`, `get`, `list`, `update`, `delete`, and
kind-specific verbs.

3.1.2 Exceptions, as top-level commands: `init`, `join`, `login`, `logout`,
`doctor`, `version`, `context`.

3.1.3 Every flag has a default wherever a sensible one exists. `alloy instance
create` with no flags must produce a running instance in the default VPC.

3.1.4 Output is a table for people by default, and `--output json` or `yaml`
gives the API representation unchanged. Output to a pipe never contains color or
progress drawing.

3.1.5 Commands that start long work wait and show progress by default. `--no-wait`
returns the operation.

3.1.6 Destructive commands ask for confirmation by name on a terminal and require
`--yes` otherwise.

3.1.7 Exit codes: 0 success, 1 failure, 2 usage error, 3 not found, 4 permission
denied, 5 conflict, 6 unavailable.

3.1.8 Errors print the API error's message, then a line starting `Try:` with the
next command when one exists.

3.1.9 Configuration is in `$XDG_CONFIG_HOME/alloy/` or the platform equivalent.
Credentials are stored in the operating system keychain where one exists,
otherwise in a file readable only by the user.

3.1.10 Shell completion for bash, zsh, fish, and PowerShell is generated from the
same definitions as the commands.

### 3.2 Console

3.2.1 The console offers every operation the CLI does for tenant resources, and
cluster views for cloud admins.

3.2.2 It uses only the public API with the user's session. It has no private
endpoints.

3.2.3 Every screen that creates or changes something can show the equivalent CLI
command.

3.2.4 It works in the current and previous major versions of Chromium, Firefox,
and Safari, and meets WCAG 2.2 level AA.

3.2.5 Standing warnings (single anchor, unprotected pool, relay-only node, expiring
records) are visible on the cluster overview and on the affected resource.

## 6. Failure behavior

The CLI retries idempotent calls on `Unavailable` and `RateLimited` with backoff
up to 30 seconds, and reuses the idempotency key on retried mutations. The console
shows stale data as stale, with its age.

## 7. Limits

CLI startup to first byte of output under 50 ms without network. Console initial
load under 300 KiB compressed.

## 8. Security

The CLI pins the cluster's CA fingerprint on first login and refuses a changed
one. Upholds I-22.

## 9. Open questions

- Command wording and the init and join experience. Task T-0013.
- UI framework choice. Research task to be cut in M9.
