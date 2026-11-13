# ARFC 0019: CLI and console

- Status: draft
- Depends on: 0002, 0004, 0006, 0007, 0008, 0017
- Crates: alloy (binary), ui/console
- Decisions: 0002, 0040, 0041, 0044, 0062

## 1. Purpose

The two surfaces people use directly. Both are plain clients of the native API.
This ARFC fixes their structure. Their wording and flows come from product briefs,
starting with PB-0001 (task T-0013).

## 2. Model

The CLI is one binary, `alloy`, for Linux, macOS, and Windows, installed and
removed by the scripts of ARFC 0017 clauses 3.15 and 3.16. On Linux and Windows
the same install also brings the **mesh client**: `alloyd`, with which `alloy
join` makes the computer a node, a client node on Windows (ARFC 0002 section
3.11). On Windows `alloy join` and `alloy node leave` need an elevated
administrator where Linux needs root.

The CLI holds **contexts**: a name, one to four API endpoints, a pinned API CA
fingerprint (`api_ca_fp`, ARFC 0002 section 3.7) or public name, an access key,
and a default org and project. At most one context is current.

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
next command when one exists. Warnings take the same form: a line starting
`Warning:`, then `Try:` when a command removes the cause. Errors and warnings go
to standard error, so standard output under `--output json` or `yaml` carries
only the API representation.

3.1.9 Configuration is in `$XDG_CONFIG_HOME/alloy/` or the platform equivalent.
Credentials are stored in the operating system keychain where one exists,
otherwise in a file readable only by the user.

3.1.10 Shell completion for bash, zsh, fish, and PowerShell is generated from the
same definitions as the commands.

3.1.11 `alloy login` has four forms. A login code or certificate that fails a
check in ARFC 0002 section 3.7 ends the command before any request is sent. A
certificate mismatch exits 1 with no `Try:` line, since no command makes it
safe. No endpoint answering within 10 seconds exits 6.

| Form | Behavior |
|---|---|
| `alloy login <login code> [--context <name>]` | Logs in to the context named by `--context`, else by the first endpoint's address, with `:<port>` added when the port is not 443 and an IPv6 address then written in brackets, as in `[fd00::1]:8443`. Exchanges the code (ARFC 0002 section 3.7), sending as `name` the hostname by the rule of ARFC 0007 clause 3.9.3, and as `replaces` the id of the access key that context holds, when it holds one (ARFC 0007 clause 3.9.10). Stores the returned key as 3.1.9 says. Creates or replaces the context with every endpoint in the code, `api_ca_fp`, the key, and the organization and project from the exchange, no default project when the exchange returns none. Makes it current, and prints the user, the organization, and the key name the exchange returned |
| `alloy login <address> [--context <name>]` | Connects to the API at the address. Picks the context named by `--context`, else the one context whose pinned fingerprint matches the presented certificate authority. Replaces that context's endpoints with the address, keeps its access key, and makes it current. Several matching contexts and no `--context` is a usage error. A named context that does not match, or no match while contexts exist, is a certificate mismatch. No context at all exits 4 as not logged in |
| `alloy login` | With a current context, confirms its access key with the API and prints the principal. With none, exits 4 as not logged in |
| `alloy login --issue-code` | Calls `IssueLoginCode` (ARFC 0002 section 3.7) for the current context's user and prints the new code once. With no current context, it follows 3.1.14 |

3.1.12 Node commands:

| Command | Behavior |
|---|---|
| `alloy node list` | Prints every node as clause 3.1.15 says |
| `alloy node get <name>` | Calls `GetNode` and prints the `Node` (ARFC 0006 section 2.6) as a view of one field per line, name and state first, then the other fields in the order of that section. Exits 3 when no node has the name |
| `alloy node approve <name>` | Calls `ApproveNode` (ARFC 0002 section 3.3). Fails with `Conflict` for a node that is not `pending` |
| `alloy node reject <name>` | Calls `RejectNode`. Fails with `Conflict` for a node that is not `pending` |
| `alloy node update <name>` | Takes `--roles <list>` and `--advertise <addr>[,...]`, for any node. Adding `anchor` or `control` to a node with no advertise address fails as ARFC 0002 section 3.10 says unless `--advertise` is given in the same command |
| `alloy node leave` | As ARFC 0002 section 3.9 says |

3.1.13 Join tokens and login codes are printed only by the command that creates
them, once, on standard output. `alloy login` stores the access key secret that
the exchange returns and never prints it. None of the three appear in logs, in
error messages, or in `--output json` or `yaml` (I-22). `--output json` or
`yaml` on a command that prints a join token or login code is a usage error.

3.1.14 Local admin. A command run with no current context, by user id 0, on a
machine where the local admin socket exists (ARFC 0007 clause 3.10.2), is sent
through that socket as the local admin when it is one of:

| Command | Local admin action (ARFC 0007 clause 3.10.1) |
|---|---|
| `alloy login --issue-code [--user <name>] [--org <name>]` | `iam:IssueLoginCode` for that user, default `admin` in org `default`. The CLI generates the secret as for any login code |
| `alloy token create` | `cluster:CreateJoinToken` |
| `alloy node update <name>` | `cluster:UpdateNode`, where `<name>` is this machine's own node |

`--user` and `--org` are accepted on this path only, and are a usage error on
any other. A refusal through the socket exits 4. Any other command with no
current context, and any of these three run by another user or on a machine
without the socket, exits 4 as not logged in.

3.1.15 Node list. `alloy node list` calls `ListNodes` (ARFC 0008 clause 3.4) and
follows every page before it prints anything. It prints one row per `Node` (ARFC
0006 section 2.6), sorted by name in ascending byte order, under these columns
in this order:

| Column | Value |
|---|---|
| `NAME` | The node's name |
| `STATE` | `pending`, `joining`, `active`, or `unreachable` (ARFC 0006 clause 3.15) |
| `ROLES` | Its roles in the order `anchor`, `control`, `compute`, `storage`, joined by commas with no space, or `-` for a client node |
| `ZONE` | Its zone. The column is printed only when the listed nodes are in more than one zone |
| `MESH` | `direct` or `relay-only` when the `Node` has `mesh` set, else `-` |
| `RELEASE` | Its running release (ARFC 0017 clause 3.11), else `-` |
| `ADDRESS` | Its first advertise address. For a node with none, as a client node often is, the first address of its latest heartbeat (ARFC 0006 clause 3.13). Else `-` |

The header row is the column names. Each column is as wide as its widest value
or name, values are left aligned, two spaces separate columns, and no line ends
in a space. Nothing is shortened. The default columns fit T20's 100 columns in
this reference case: every node name 20 characters long, one zone, a node with
all four roles, state `active`, mesh `direct`, a five-character release, and a
15-character IPv4 address, which gives lines of 94 columns. Each wider value
widens its column: `unreachable` by 5, `relay-only` by 4, and, with more than
one zone, the ZONE column adds 2 plus the width of the longest zone name, at
least 4, so zone names of more than 4 characters pass 100 columns. A cluster
always has a node, so the list is never empty. Standard output carries only the
table. On a terminal, with color on, the header is bold, `unreachable` is red, and
`pending` is yellow, and nothing else is colored. After the table the CLI prints
the standing warnings that concern nodes on standard error (3.2.5). The table of
a three-node cluster looks like this:

```
NAME     STATE   ROLES                           MESH  RELEASE          ADDRESS
laptop   active  -                               -     0.0.1-preview.2  192.168.1.50
linux-1  active  anchor,control,compute,storage  -     0.0.1-preview.2  192.168.1.20
linux-2  active  compute,storage                 -     0.0.1-preview.2  192.168.1.21
```

With `--output json` or `yaml` it prints, on standard output, one array of every
`Node` of every page in the API's representation (ARFC 0008 section 2), in the
order `ListNodes` returned them, and no standing warning.

### 3.2 Console

3.2.1 The console offers every operation the CLI does for tenant resources, and
cluster views for cloud admins.

3.2.2 It uses only the public API with the user's session. It has no private
endpoints.

3.2.3 Every screen that creates or changes something can show the equivalent CLI
command.

3.2.4 It works in the current and previous major versions of Chromium, Firefox,
and Safari, and meets WCAG 2.2 level AA.

3.2.5 Standing warnings (single anchor, unprotected pool, relay-only node,
expiring records, an advertise address that is gone (ARFC 0002 section 3.10),
nodes on two releases) are visible on the cluster overview and on the affected
resource. The API reports each one with its kind, the affected resource, a
message, and the command that removes it. The console and the CLI show that list
and derive none themselves. The CLI prints the standing warnings that concern
nodes after the table of `alloy node list`, and every standing warning after
`alloy init` and `alloy join`, each in the form of 3.1.8 on standard error.

## 6. Failure behavior

The CLI retries idempotent calls on `Unavailable` and `RateLimited` with backoff
up to 30 seconds, and reuses the idempotency key on retried mutations. The
console shows stale data as stale, with its age.

## 7. Limits

CLI startup to first byte of output under 50 ms without network. Console initial
load under 300 KiB compressed.

## 8. Security

The CLI pins the API CA fingerprint carried in the login code, with no trust on
first use, and refuses a changed one. Upholds I-22.

## 9. Open questions

- UI framework choice. Research task to be cut in M9.
