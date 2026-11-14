# ARFC 0019: CLI and console

- Status: draft
- Depends on: 0002, 0004, 0006, 0007, 0008, 0017
- Crates: alloy (binary), ui/console
- Decisions: 0002, 0040, 0041, 0044, 0062, 0070

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
is written in TypeScript, built into static files embedded in `alloyd`, and
calls the API's Connect form (ARFC 0008 clause 3.14) with a client generated
from the API's definitions (ADR 0070). A browser reaches it in one of two ways.
A cluster with a public name presents an ACME certificate, so a browser opens
`https://api.<cluster-domain>/` directly and logs in with a login code. A
cluster without one presents a certificate from its own API CA, which no
browser trusts, and is reached through the **console bridge**: `alloy console`
listens on `127.0.0.1`, connects to the API with the context's pinned API CA,
holds a session token for the context's user, and forwards the browser's
requests (3.1.16). The console never asks a user to accept a certificate
warning.

## 3. Behavior

### 3.1 CLI grammar

3.1.1 Commands are `alloy <noun> <verb> [name] [flags]`: `alloy instance create
web --image ubuntu-24.04 --shape small`. Nouns are the resource kinds in their
glossary spelling. Verbs are `create`, `get`, `list`, `update`, `delete`, and
kind-specific verbs.

3.1.2 Exceptions, as top-level commands: `init`, `join`, `login`, `logout`,
`console`, `doctor`, `version`, `context`.

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
| `alloy login` | With a current context, confirms its access key with the API by calling `GetCaller` (ARFC 0008 clause 3.16) and prints the principal. With none, exits 4 as not logged in |
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

3.1.16 Console bridge. `alloy console [<link>] [--no-browser] [--context
<name>]` runs the console bridge for the current context, or the one
`--context` names, until it is stopped.

1. It connects to the context's endpoints in order, for at most 10 seconds in
   all, and verifies the API certificate as ARFC 0008 section 3.8 says. A
   certificate mismatch exits 1 as in 3.1.11, with the browser not opened, and
   no endpoint answering exits 6. No current context exits 4 as not logged in.
2. It calls `CreateSessionToken` (ARFC 0007 clause 3.12.1) signed with the
   context's access key, without `cookie`, and keeps the token in memory only.
   An error exits with the code's exit code (3.1.7) and its message.
3. It listens on TCP on `127.0.0.1`, on a port the operating system chooses,
   and serves HTTP/1.1 there.
4. It draws a launch secret of 32 bytes from the `alloy-rt` randomness trait,
   encoded as unpadded base64url. The **launch address** is
   `http://127.0.0.1:<port>/#alloy-launch=<secret>`. The secret is in the
   fragment, which a browser sends in no request (RFC 3986 section 3.5,
   https://www.rfc-editor.org/rfc/rfc3986#section-3.5).
5. It writes the **launch file** `console-<port>.html` in the directory of
   3.1.9, created readable and writable only by the user: mode `0600` on Linux
   and macOS, and on Windows an access control list that grants only the user.
   The file is an HTML document that holds
   `<meta http-equiv="refresh" content="0; url=<launch address>">` and a link
   to the launch address, and nothing else. The bridge opens the system's
   default browser at the file's `file://` address, so the secret is in no
   process argument, which other users of the computer can read (on Linux,
   `/proc/<pid>/cmdline`, https://man7.org/linux/man-pages/man5/proc_pid_cmdline.5.html).
   Jupyter Notebook opens its browser through such a file for the same reason
   (`use_redirect_file`, https://jupyter-notebook.readthedocs.io/en/stable/config.html).
   The bridge deletes the file when the secret is spent, 120 seconds after the
   start, or when it exits, whichever comes first. With `--no-browser` it
   writes no file and opens nothing, and when no browser opens it deletes the
   file. In both cases it prints the launch address instead of the plain one.

It prints, on standard error, `Opening the console for context <name>`, then
`  ok    API certificate matches the pinned authority` (or, for a public name,
`  ok    API certificate is valid for <name>`), then `  done  browser session
for <user>`; on standard output the plain address `http://127.0.0.1:<port>/`
(or the launch address, as step 5 says); and on standard error `Opened in your
browser. The console works while this command runs. Stop it with Ctrl-C.`, or,
when it opened nothing, `Open this address in a browser on this computer. It
works once, within 120 seconds.`

The bridge serves requests by these rules, which it checks in this order:

| Request | Answer |
|---|---|
| `Host` is not exactly `127.0.0.1:<port>` | HTTP status 421, an empty body, nothing forwarded |
| `POST /-/launch` whose `Origin` is `http://127.0.0.1:<port>` and whose body is exactly the launch secret, its first use, within 120 seconds of the start | HTTP status 200, `content-type: application/json`, `cache-control: no-store`, and the body `{"credential": "<value>", "landing": "<landing path>"}`, where `<value>` is the **bridge credential**: 32 bytes from the randomness trait as unpadded base64url, drawn once per bridge. The secret is then spent |
| Any other request to a path under `/-/` | HTTP status 404 and the text `This launch address was already used or has expired. Run alloy console again.` |
| A path under `/alloy.` that is not `POST`, lacks `connect-protocol-version: 1`, carries an `Origin` other than `http://127.0.0.1:<port>`, or lacks the header `alloy-bridge` with the bridge credential as its value | Connect code `unauthenticated`, HTTP status 401 (ARFC 0008 clause 3.14.4), nothing forwarded |
| Any other path under `/alloy.` | Forwarded, with `authorization: Bearer <token>` |
| Anything else | Forwarded with no credential |

The bridge sets no cookie. The landing path is the path and query of `<link>`,
else `/`. Before it forwards a request the bridge removes every
`authorization`, `cookie`, and `alloy-bridge` header and every header whose
name starts with `x-amz-`, and from every response it removes every
`set-cookie` header. It adds to every response the header
`alloy-api-address` with the `https://` address of the endpoint it is connected
to, which the console uses to copy links (3.2.10). It forwards over one HTTP/2
connection to the API, and when that breaks, reconnects as step 1 does, retrying
with backoff from 1 to 30 seconds and answering forwarded requests meanwhile
with Connect code `unavailable` and HTTP status 503. When less than 60 minutes
of its token's life remain it calls `RefreshSessionToken`, and when the session
has reached 30 days it creates a new one as step 2 does.

`<link>` is an `https` address whose host, and port when it has one, are those
of one of the context's endpoints or its public name, and whose path is empty,
or is `/`, or starts with `/` followed by a character other than `/` and `\`,
and does not start with `/-/`. So the landing path is a path of the bridge's
own origin, never a network-path reference to another host (RFC 3986 section
4.2). Any other link is a usage error that names the context's addresses and
says that the path must be a path of the console.

On Ctrl-C, or the platform's termination signal, the bridge stops accepting
connections, calls `RevokeSessionToken` for its session, waiting at most 5
seconds, prints `Stopped the console bridge. The browser session ended with
it.` on standard error, and exits 0. When the revocation fails it prints
instead `Warning: the browser session could not be ended at the API. It ends by
<time>.`, with the token's expiry, and exits 0. When a `RevokeSessionToken` it
forwarded succeeds, it answers every later request under `/alloy.` with Connect
code `unauthenticated`, prints `Stopped the console bridge: the console logged
out.`, and exits 0.

### 3.2 Console

3.2.1 The console offers every operation the CLI does for tenant resources, and
cluster views for cloud admins.

3.2.2 It uses only the public API with the user's session. It has no private
endpoints.

3.2.3 Every screen that creates or changes something can show the equivalent CLI
command, with no request. The mapping from each API method and its fields to a
CLI command and its flags is generated from the same definitions as the CLI
(3.1.10) and built into the console. An action whose CLI command this ARFC
does not yet specify shows the command that 3.1.1 forms from its kind and verb,
and is provisional until that command is specified (section 9).

3.2.4 It works in the current and previous major versions of Chromium, Firefox,
and Safari, and meets WCAG 2.2 level AA.

3.2.5 Standing warnings (single anchor, unprotected pool, relay-only node,
expiring records, an advertise address that is gone (ARFC 0002 section 3.10),
nodes on two releases) are visible on the cluster overview and on the affected
resource. The API reports each one with its kind, the affected resource, a
message, and the command that removes it. The console and the CLI show that list
and derive none themselves. The CLI prints the standing warnings that concern
nodes after the table of `alloy node list`, and every standing warning after
`alloy init` and `alloy join`, each in the form of 3.1.8 on standard error. The
API's list is `ListStandingWarnings` (ARFC 0008 clause 3.17).

3.2.6 Serving. Every control node serves the console of its own release at its
API port (ARFC 0008 clause 3.12.1). `GET` of a path under `/assets/` returns that
file of the console's build, with `cache-control: public, max-age=31536000,
immutable`, since every asset's name carries a hash of its content, and HTTP
status 404 when there is no such file. Any other `GET` returns the console's
index document with `cache-control: no-cache`. Paths under `/-/` are the
bridge's: `alloyd` returns HTTP status 404 for them. Each file is served gzip
compressed, with `content-encoding: gzip`, when the request's `accept-encoding`
includes `gzip`, and as is otherwise. Every response of the console's files
carries these headers, and a public name adds
`strict-transport-security: max-age=31536000`:

| Header | Value |
|---|---|
| `content-security-policy` | `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'` |
| `x-content-type-options` | `nosniff` |
| `referrer-policy` | `no-referrer` |
| `cross-origin-opener-policy` | `same-origin` |

The console's files hold no inline script and no inline style, and load
nothing from any other origin. They need no authentication, since they hold no
data.

3.2.7 Session in the browser. At a public name the console's login screen
takes a login code, checks its version and checksum (ARFC 0002 section 3.7)
before any request, and calls `CreateSessionToken` with its `code_id` and
secret and `cookie` set, so page script never holds the token (ARFC 0008
clause 3.14.6).
While the console is open it calls `RefreshSessionToken` with `cookie` set
when less than 60 minutes of the token's life remain, as `GetCaller` reports
it. `Log out` calls `RevokeSessionToken`. The console knows it runs through the
bridge when its origin is `http://127.0.0.1:<port>`. There the browser holds
only the bridge credential (3.1.16), never a token, and the console neither
logs in nor refreshes: the bridge does both. When its address has the fragment
`#alloy-launch=<secret>`, the console first replaces its address with `/`
without the fragment, then sends `POST /-/launch` with the secret as the body,
keeps the returned credential in the origin's `localStorage` under the key
`alloy-bridge`, and moves to the returned landing path, or to `/` when that
path does not start with `/` followed by a character other than `/` and `\`.
It sends the credential as the header `alloy-bridge` on every call under
`/alloy.`. Web storage is kept per origin, and an origin includes its port
(https://html.spec.whatwg.org/multipage/webstorage.html), so every tab of one
bridge shares the credential, and no other port on `127.0.0.1` receives it.
Without a stored credential, or when `POST /-/launch` fails, the console shows
the text of that failure, or `Run alloy console to open the console.`, with the
command to copy.

3.2.8 API use. The console calls only the Connect form, only with `POST`, and
on start calls `GetCaller` (ARFC 0008 clause 3.16), which chooses its
navigation: the cluster screens only for a holder of a cluster role. It holds
at most 3 Watch streams open at once: one for the visible screen, one for
standing warnings when the caller holds a cluster role, and one for the
current project's operations. Through the bridge the browser speaks HTTP/1.1,
and Chromium opens at most 6 connections per host for it
(https://chromium.googlesource.com/chromium/src/+/main/net/socket/client_socket_pool_manager.cc),
so 3 are left for other calls. Every mutating call sets a new idempotency key
and reuses it on each retry (rule A14).

3.2.9 Keyboard. Single-character keys act only when focus is not in a text
field, and Settings turns them off. `?` shows this table.

| Key | Does |
|---|---|
| `Ctrl+K`, `Cmd+K` on macOS | Opens the command palette |
| `?` | Shows the shortcut list |
| `Esc` | Closes the topmost dialog, palette, or menu. In a list, clears the selection |
| `g` then `h`, `n`, `m`, `i`, `v`, `o`, `s` | Goes to overview, nodes, mesh, instances, VPCs, operations, settings |
| `j`, `k`, or the arrow keys | Moves the focused row |
| `Enter` | Opens the focused row |
| `/` | Focuses the filter field |
| `c` | Create, on a list screen |
| `.` | Opens the actions menu of the focused row or the open resource |
| `Shift+D` | Deletes the focused or open resource, after asking for its name |
| `y` | Copies the focused resource's full name |
| `Shift+Y` | Copies the CLI command of the open dialog, or the `get` command of the focused row |
| `Ctrl+Enter`, `Cmd+Enter` | Submits the open form |
| `p` | Switches project |
| `[` | Collapses or expands the navigation column |
| `1` to `9` | Selects that tab of a detail screen |

The palette takes its first keystroke within 100 ms of opening and filters with
no network request. It lists, in order, the actions of the current screen and
its focused row, the `Go to` entries, the resources already loaded by name, and
`Switch project`, `Switch theme`, `Show shortcuts`, `Copy link`, `Copy CLI
command`, `Issue login code`, and `Log out`. Each action entry shows its
shortcut, if any, and its CLI command.

3.2.10 URLs. A screen's path is the full name of what it shows (rule A2).
Filters, order, and tab are the query parameters `filter`, `order_by`, and
`tab`.

| Screen | Path |
|---|---|
| Overview | `/overview` |
| Nodes, a node | `/nodes`, `/nodes/<name>` |
| Join tokens | `/join-tokens` |
| Mesh | `/mesh` |
| Instances, an instance | `/orgs/<org>/projects/<project>/instances`, `.../instances/<name>` |
| VPCs, a VPC, a subnet | `.../vpcs`, `.../vpcs/<name>`, `.../vpcs/<name>/subnets/<name>` |
| A project's operations, an operation | `/orgs/<org>/projects/<project>/operations`, `.../operations/<name>` |
| An org's operations, an operation | `/orgs/<org>/operations`, `/orgs/<org>/operations/<name>` |
| Cluster operations, an operation | `/operations`, `/operations/<name>` |
| Login, settings | `/login`, `/settings` |

`/` lands a holder of a cluster role on `/overview` and anyone else on the
instances of the caller's default project. `Copy link` copies the address of
the API itself with the screen's path and query, never the bridge's: through
the bridge, the `alloy-api-address` of its responses (3.1.16).

3.2.11 Live updates. Each visible list and detail screen keeps one Watch (ARFC
0008 clause 3.15) open, and shows its rows change in place, with no refresh
control. A change the user makes shows `pending` until the Watch delivers the
resource with a higher `generation`, and an operation shows its stages until
it is done. A broken stream (ARFC 0008 clause 3.15.5), and a Watch call that
fails with `Unavailable` or `RateLimited`, whichever control node answered, is
called again with backoff from 1 to 30 seconds, and no shorter than the error's
retry delay, from the last complete revision (ARFC 0008 clause 3.15.3). While a screen's
stream is broken, the top bar shows `Stale, <age>` with the age since its last
message, and the console refuses changes with the same words. `Compacted`
makes the console watch from revision 0 and replace the screen's data.

3.2.12 Releases. The console compares its own release with the
`alloy-release` header of each API response (ARFC 0008 clause 3.12.1). When
they differ it shows `This console is release <its> and the API answered as
<the API's>. Reload to get the matching console.` with a `Reload` action.

3.2.13 Feel. The console meets rules C1 to C19 of `product/principles.md`,
section Web console, each as a requirement of this clause. A screen whose data
is loaded renders in under 100 ms, and every input is acknowledged on screen in
under 100 ms.

## 6. Failure behavior

The CLI retries idempotent calls on `Unavailable` and `RateLimited` with backoff
up to 30 seconds, and reuses the idempotency key on retried mutations. The
console shows stale data as stale, with its age (3.2.11), and retries as the
CLI does.

When a call fails with `Unauthenticated` the console shows `Your session ended.
Log in again to continue.`, keeps unsent form input, and offers `Log in` at a
public name or the command `alloy console` through the bridge. When the bridge
stops answering, the console shows `The console bridge stopped. Run alloy
console again.` with that command to copy.

## 7. Limits

CLI startup to first byte of output under 50 ms without network. Console initial
load under 300 KiB compressed: the index document and every script and
stylesheet it loads before the first screen renders, gzip compressed, sum to at
most 307,200 bytes (ADR 0070). At most 3 open Watch streams per console tab
(3.2.8).

The bridge accepts at most 32 open connections and closes any one more at
once. A request's headers are at most 64 KiB and its body at most 4 MiB and 5
bytes, and a larger one gets HTTP status 431 or 413 and is not forwarded. The
launch secret works once, within 120 seconds of the start, and the launch file
exists at most that long. One bridge credential per bridge.

## 8. Security

The CLI pins the API CA fingerprint carried in the login code, with no trust on
first use, and refuses a changed one. The console bridge uses the same pin, so
a private cluster needs no change to any trust store.

The bridge. Only processes on the same computer reach `127.0.0.1`. The `Host`
check refuses a request that a page of another name sent there through DNS
rebinding. The context's access key never leaves the CLI: the bridge forwards
a session token that derives from it (ARFC 0007 clause 3.12.1), so revoking
the key ends the session, and stopping the bridge revokes it.

The bridge defends the user's session against two parties: other accounts on
the same computer, and pages of other origins in the user's browser, servers
on other ports of `127.0.0.1` among them. Another account can connect to the
bridge's port but holds no credential the bridge accepts. The launch secret
reaches only the user's own browser: it is in no process argument, the launch
file is readable only by the user (3.1.16), and a browser sends no fragment in
a request. The bridge credential travels only in the body of the one answer to
`POST /-/launch` and in the `alloy-bridge` header that the console's script
sends to its own origin. It is no cookie, since cookies are not separated by
port (RFC 6265 section 8.5, https://www.rfc-editor.org/rfc/rfc6265#section-8.5),
so no server on another port receives it, and a second bridge on another port
keeps its own and leaves the first working. A page of another origin cannot
send a call the bridge forwards: it lacks the credential, and the header
`alloy-bridge` and `connect-protocol-version` need a preflight, which the
bridge never answers with CORS headers. The launch secret works once and for
120 seconds, so a copy in the browser's history opens nothing. Page script can
read the bridge credential, as it could not read an `HttpOnly` cookie, and the
content security policy (3.2.6) is what keeps foreign script from it. The value
works only from this computer, against this bridge, while it runs.

The bridge does not defend against the user's own account or an administrator
of the computer: either can read the context's access key (3.1.9), the
bridge's memory, and the browser's storage.

The console at a public name keeps its session token in the cookie of ARFC
0008 clause 3.14.6, and its content security policy (3.2.6) lets no injected
or foreign script run and no other site frame it. Upholds I-02, I-22.

## 9. Open questions

- CLI commands for `instance`, `vpc`, `subnet`, `security-group`, `operation`,
  and `mesh path`, which the console's Instances, VPCs, Operations, and Mesh
  actions need (3.2.3). Each comes with its kind's spec task.
- The API reports no paths between nodes and no relay throughput, which the
  Mesh screen needs (ARFC 0004 sections 3.7 and 3.8, ARFC 0006 section 2.6), and
  the serial console of ARFC 0011 clause 3.1.5 streams from the client, which a
  browser cannot do. Raised by T-0139 for spec tasks on those ARFCs.
