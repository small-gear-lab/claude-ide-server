# Design

Status: draft. Nothing here is implemented yet. Statements about Claude Code's behaviour come from observing
existing integrations and from public write-ups; they are marked **(assumption)** where they have not been
checked against Claude Code itself.

## Goals

- Let Claude Code's `/ide` connect to any editor, by separating the part that speaks Claude Code's protocol
  from the part that touches an editor.
- Keep the server free of any knowledge of a particular editor. An editor adapter is a small program (or a
  plugin) that speaks JSON-RPC to the server.
- Support several workspaces, each with its own lock file and WebSocket port, from one server process.
- Clean up after itself: when an editor goes away, so do its lock file and port.

## Non-goals

- Starting or managing the Claude Code CLI or its terminal. The CLI runs wherever the user runs it; `/ide`
  finds the server through the lock file.
- Windows. The adapter socket is a Unix domain socket. A named-pipe transport can be added later.
- Replacing the official integrations. This is for editors that do not have one.
- A hub entry that stands for all workspaces. Considered and left out: a CLI normally runs inside the project that its editor has open, and a second entry that matches the same directory would make `/ide` ambiguous.

## Overview

```
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix socket)── editor adapter
   (1 per CLI)                    (1 process)                                    (1 per workspace)
```

The server has two faces:

- **Claude side.** For each registered workspace, it listens on a loopback WebSocket port, writes a lock file,
  authenticates the CLI with a token, and speaks MCP (JSON-RPC 2.0): it answers `initialize`, `tools/list` and
  `tools/call`, and sends notifications.
- **Adapter side.** It listens on one Unix socket. An adapter connects, registers a workspace, pushes
  notifications (selection changed, diagnostics changed) and answers the server's requests (open a file, get
  the open editors, show a diff).

## The Claude side

What is observed of the existing integrations:

- The IDE listens on a random port in 10000–65535, bound to `127.0.0.1` only.
- It writes `<port>.lock` into `~/.claude/ide/` (or `$CLAUDE_CONFIG_DIR/ide/`) containing
  `pid`, `workspaceFolders`, `ideName`, `transport` (`"ws"`), `authToken`, and (JetBrains) `runningInWindows`.
  The file is mode 0600 and the directory 0700.
- The CLI connects over WebSocket with the header `x-claude-code-ide-authorization: <authToken>`. A mismatch is
  closed (code 1008).
- Messages are JSON-RPC 2.0, based on MCP. (The protocol notes of other implementations say 2025-03-26; the CLI
  below asked for a newer version.)
- The CLI sends `initialize`, then the notification `ide_connected` with its pid.

### What a real CLI sends

Observed by connecting Claude Code 2.1.322 to a probe that answers minimally (`crates/core/examples/mcp_probe.rs`):

- **The WebSocket upgrade offers the subprotocol `mcp`, and the CLI gives up if the server does not pick one.** The
  first connection attempts failed for exactly this reason: the CLI connected and dropped the connection at once,
  without sending a message. The server therefore answers with the first subprotocol that the client offered.
- Then, in this order: the request `initialize`, the notification `notifications/initialized`, the notification
  `ide_connected` (`{ "pid": ... }`), and the request `tools/list`.
- `initialize` asks for `protocolVersion` `2025-11-25`, with `capabilities` of `roots` (`listChanged`), `elicitation`
  and an `extensions` entry for tasks, and `clientInfo` (`name` `claude-code`, `title`, `version`, `description`,
  `websiteUrl`). Answering with the same `protocolVersion`, `capabilities: { "tools": {} }` and a `serverInfo` was
  enough: the CLI went on to `tools/list`.
- With that, `/ide` lists the probe by its `ideName` and shows it as connected.

### How the CLI picks a lock file

Observed in Claude Code 2.1.322, by reading the installed CLI:

- **Matching.** The CLI compares its working directory with each folder of `workspaceFolders` (both normalised
  to Unicode NFC). A lock file is offered when a folder contains the working directory (equal to it, or an
  ancestor of it). A folder that lies inside the working directory counts as a weaker match, under a condition
  that is not known. Anything else is not offered. The comparison is on the text of the paths; no resolving of
  symlinks was seen, so write `workspaceFolders` as the paths the user would `cd` to.
- **The port variable.** A lock file whose port equals `CLAUDE_CODE_SSE_PORT` is always offered, whatever its
  folders.
- **Order.** Lock files are read newest first (by modification time). `/ide` lists the matches; when several
  have the same `ideName`, the list shows their folders to tell them apart. Auto-connect takes the first.
- **`pid`.** The CLI removes a lock file whose `pid` is not a live process on the machine (`kill(pid, 0)`), or
  that cannot be read, or that has no `pid` and whose port does not accept a TCP connection. So `pid` must be the
  adapter's own live process, and a lock file that outlives it is deleted by the next CLI that looks.
- **Inside an IDE terminal.** When the CLI runs in a VS Code or JetBrains terminal (or with
  `FORCE_CODE_TERMINAL` set), it also requires the lock file's `pid` to be its parent process or an ancestor
  (up to 10 levels), unless the port equals `CLAUDE_CODE_SSE_PORT`. In an ordinary terminal there is no such
  check. A Neovim `:terminal` satisfies it, because Neovim is an ancestor of the shell.
- **Transport.** `transport` equal to `"ws"` means a WebSocket at `ws://<host>:<port>`; anything else is treated
  as the older SSE transport at `http://<host>:<port>/sse`. The port is the lock file's name without `.lock`.
  A lock file that is not JSON is read as a plain list of folders, one per line.
- **Auto-connect.** In an ordinary terminal the CLI connects only when asked (`/ide`) or when the user turned on
  auto-connect in its configuration; inside an IDE terminal, or with `CLAUDE_CODE_SSE_PORT` set, it connects by
  itself.

Not tested: what the CLI does when `authToken` is missing.

### Tools exposed to Claude

The set is the one the VS Code extension registers. The server exposes only the tools whose backing adapter
method the adapter declared at registration (see `register` below), the same way the JetBrains plugin exposes a
smaller set than the VS Code extension.

| MCP tool | Adapter method | Notes |
|---|---|---|
| `openFile` | `openFile` | optional selection by `startText` / `endText` |
| `openDiff` | `openDiff` | blocks until the user accepts or rejects |
| `close_tab` | `closeDiff` | by `tab_name`, the one given to `openDiff` |
| `closeAllDiffTabs` | `closeAllDiffs` | answers `CLOSED_<n>_DIFF_TABS` |
| `getOpenEditors` | `getOpenEditors` | |
| `getCurrentSelection` | `getCurrentSelection` | |
| `getLatestSelection` | `getLatestSelection` | the last selection, even if the editor lost focus |
| `getWorkspaceFolders` | (answered by the server) | from the registration |
| `getDiagnostics` | `getDiagnostics` | one URI, or all |
| `checkDocumentDirty` | `checkDocumentDirty` | |
| `saveDocument` | `saveDocument` | |

`executeCode` (Jupyter only) is out of scope.

How a call turns out:

- The answer to a tool call is one text content. If the adapter answers with a string, that string is the text as it
  is (for example `FILE_SAVED`); anything else is written as JSON.
- A call that fails (the adapter answers with an error, does not answer in time, or is gone) is still an answer: the
  result carries `isError: true` and the reason as its text. Claude gets to see why.
- A call for a tool that is not available (unknown, or not declared by the adapter) is an error of the protocol, with
  code `-32602`, and nothing is sent to the adapter.
- `getWorkspaceFolders` is answered by the server from the registration, so it follows `workspaceFoldersChanged`.

The MCP protocol itself (the handshake, version negotiation, `tools/list`, `tools/call`, errors) is handled by the
`rmcp` crate, the official Rust SDK of MCP, behind the crate's own types in the `mcp` module. The least supported
Rust is 1.88 because `rmcp` needs it. The messages that MCP does not define (`ide_connected`, `selection_changed`, ...)
are custom notifications of the SDK.

### Notifications sent to Claude

| Notification | When |
|---|---|
| `selection_changed` | the adapter reports a new selection |
| `at_mentioned` | the user sends the selection or a file as a reference (`filePath`, `lineStart`, `lineEnd`) |
| `diagnostics_changed` | the adapter reports changed diagnostics (`{ "uris": [...] }`) |

Several integrations send `diagnostics_changed`; some third-party implementations do not. It is the first
feature this project adds beyond what is commonly available.

### Several CLIs on one workspace

Each WebSocket connection is a separate client. The server accepts more than one per workspace (the VS Code
extension keeps only the latest). A diff belongs to the connection that opened it, and is closed when that
connection closes it or goes away.

## The adapter side

### Transport

- A Unix domain socket at `$XDG_RUNTIME_DIR/claude-ide-server/adapter.sock`, in a directory of mode 0700. The
  socket file is mode 0600.
- JSON-RPC 2.0 with `Content-Length` framing, as in LSP. Editors that already have an LSP client can reuse its
  transport. Checked with Neovim 0.12: `vim.lsp.rpc.connect(path)` connects to a Unix socket, the adapter can
  send requests and notifications and answer the server's requests (the `server_request` dispatcher), and when
  the server closes the connection `on_exit` is called and `is_closing()` becomes true. A missing socket raises no
  error: Neovim prints a message and no `on_exit` follows, so the adapter checks that the socket exists before
  connecting.
- Both sides send requests and notifications. The connection is the workspace: when it closes, the server closes
  the workspace's WebSocket clients and port and removes the lock file.

### Messages

Adapter → server:

| Message | Kind | Params |
|---|---|---|
| `register` | request | `ideName`, `pid`, `workspaceFolders`, `methods` (the server→adapter methods it implements) → `{ "port": n }` |
| `workspaceFoldersChanged` | notification | `workspaceFolders` (the lock file is rewritten) |
| `selectionChanged` | notification | `filePath`, `text`, `selection` |
| `diagnosticsChanged` | notification | `uris` |
| `atMention` | notification | `filePath`, `lineStart`, `lineEnd` |

Server → adapter:

| Message | Kind | Params → result |
|---|---|---|
| `openFile` | request | `filePath`, `preview`, `startText`, `endText`, `selectToEndOfLine`, `makeFrontmost` → `{ filePath, languageId, lineCount }` |
| `getOpenEditors` | request | → `{ tabs: [{ uri, isActive, label, languageId, isDirty }] }` |
| `getCurrentSelection` | request | → selection, or `{ success: false }` |
| `getLatestSelection` | request | → selection, or `{ success: false }` |
| `getDiagnostics` | request | `uri?` → `[{ uri, diagnostics: [{ message, severity, range, source }] }]` |
| `checkDocumentDirty` | request | `filePath` → `{ isDirty, isUntitled }` |
| `saveDocument` | request | `filePath` → `{ saved }` |
| `openDiff` | request | `oldFilePath`, `newFilePath`, `newFileContents`, `tabName` → `"saved"` or `"rejected"`, answered when the user decides |
| `closeDiff` | request | `tabName` |
| `closeAllDiffs` | request | → `{ closed: n }` |
| `shutdown` | notification | the server is stopping |

A selection is `{ start: { line, character }, end: { line, character }, isEmpty }`, with `line` 0-based and
`character` in UTF-16 code units (the LSP convention), `end` exclusive. The adapter converts from the editor's
own unit. **(assumption)** The CLI only counts lines from this value, so a mistake in `character` does not
change what Claude sees.

On `"saved"` from `openDiff`, the adapter has written `newFileContents` to `newFilePath`. The server answers
Claude with the text `FILE_SAVED`; on `"rejected"`, `DIFF_REJECTED`.

### Versioning

`register` carries `protocolVersion`, an integer; the version described here is `1`. The server refuses an adapter
whose version it does not know. Methods can be added without a bump; an adapter lists in `methods` what it
implements.

### `register` in detail

- It must be the first message, and it is a request. Anything else first is answered with an error (a request) or
  just closes the connection (a notification). `register` twice is an error.
- Parameters: `protocolVersion` (integer), `ideName` (non-empty string), `pid` (positive integer), `workspaceFolders`
  (a non-empty array of strings), `methods` (an array of strings). A missing or wrong parameter is an error.
- The result is `{ "port": <integer> }`. By then the port listens and the lock file exists.
- An adapter has 10 seconds to send it; a connection that does not is closed.
- `workspaceFoldersChanged` takes `{ "workspaceFolders": [...] }` (non-empty). Anything else is ignored.
- The server's methods that an adapter has not declared in `methods` are not called. A request that the server
  does not know, sent by a registered adapter, gets `-32601`.

Error codes: the JSON-RPC ones (`-32602` for bad parameters, `-32601` for an unknown method) and three of this
protocol: `-32000` the protocol version is not supported, `-32001` the workspace could not be set up (the port or
the lock file), `-32002` the first message was not `register`, or `register` came twice. After an error to
`register` the server closes the connection.

When the server drops a workspace it sends the notification `shutdown` and closes the connection. A request that
is waiting for an answer when the connection ends fails.

## Structure of the core crate

The crate runs on tokio: its `async` functions must be called inside a tokio runtime, and tokio's I/O traits
(`AsyncRead`, `AsyncWrite`) are the common currency between layers. Other tokio types (channels, tasks) and the
types of the libraries below do not appear in the crate's public interface.

Each outside library sits behind a type of the crate's own, and what lies inside knows only those types. There are
three layers:

- **The Claude side:** the WebSocket server (`ws`), the MCP handling, the lock files (`lock`).
- **The middle:** workspaces (`workspace`), the dispatch of tool calls, and the crate's own types for tools,
  selections and diagnostics.
- **The adapter side:** the adapter transport, the `register` exchange and the requests (`adapter`), and the
  JSON-RPC framing (`jsonrpc`).

The sides do not know each other. `adapter` has no port and no lock file in it: when an adapter registers, it
hands the registration up (as a `Registering`) and waits for the answer, which is either a port or a refusal. The
`workspace` module is where the two meet: it starts the WebSocket server and writes the lock file for each
registration, gives the adapter its port, and ties their lifetimes together. `ws` and `lock` know nothing of
adapters.

Three seams are drawn on purpose, where this design already says that something may change:

1. **The adapter transport.** A Unix socket now, a named pipe later. Everything above it works with a duplex byte
   stream that a listener accepts, not with a Unix socket type.
2. **The MCP SDK.** If `rmcp` is used, its types stay inside the Claude side. The middle and the adapter side use
   the crate's own types, so that a new version of the SDK, or a different SDK, touches one layer.
3. **The file system and the environment.** The lock file directory, the runtime directory and the check that a
   `pid` is alive are passed in as configuration. Tests pass temporary directories.

Nothing else is put behind a trait in advance (TCP itself, the JSON shape of a lock file). A seam is added when
something is known to change, not before.

One thing is kept out of the seams on purpose: the source of the token. The token is the only thing that keeps the
other processes of the machine away from a workspace, so it always comes from the operating system's random source,
and if that fails the workspace is refused instead of getting a weaker token. It is not part of the configuration
that a caller can change. Tests can replace it (to try the failing case, or to know the token in advance) through the
`test-support` feature of the crate, which is on only for the crate's own tests and never in a normal build.

## Lifecycle

1. The server starts and listens on the adapter socket.
2. An adapter connects and sends `register`.
3. The server picks a free port on `127.0.0.1`, generates a 128-bit token from the OS CSPRNG, writes the lock
   file atomically (write to a temporary name, then rename), and replies with the port.
4. The CLI finds the lock file, connects, and authenticates. Selection and diagnostics flow to it; its tool
   calls become adapter requests.
5. The adapter disconnects (or crashes): the server closes the workspace's connections, removes the lock file,
   and fails any pending requests.
6. On shutdown the server sends `shutdown` to adapters and removes every lock file it wrote.

If the server itself is killed, its lock files are left behind. The server records the lock files it wrote
under its runtime directory and removes the ones that are still there on the next start.

## Security

- Every listener is on `127.0.0.1`. The adapter socket is in a 0700 directory and is itself 0600.
- The token is checked on every WebSocket upgrade. A web page cannot set that header, so a browser cannot talk
  to the port.
- Frame and message sizes are capped. A malformed message closes that connection and nothing else.
- Paths from Claude are used as given by the adapter; the server never reads or writes project files itself.
  Writing a file after an accepted diff is the adapter's job.

## Testing

- A mock Claude client (WebSocket) and a mock adapter (Unix socket) run in the same test process. Tests cover the
  handshake, authentication, every tool, notifications, diffs, disconnects and cleanup. The parts that the tests
  share are in `crates/core/tests/common/`: the adapter's end of a connection (`Client`), a workspace server on
  temporary directories (`Fixture`), Claude Code's client (`MockClaude`), and a workspace with both already
  connected (`Scene`). Most tests run over in-memory streams; one runs the whole path on a real Unix socket
  (`tests/end_to_end.rs`: register, connect with the token from the lock file, list the tools, call one, disconnect,
  clean up).
- The expected shapes of what the server sends and writes come from the observed behaviour above, written down as
  fixtures in `crates/core/tests/fixtures/`: the `initialize` that Claude Code sends, and the shapes of a lock file and
  of the results of `initialize`, `tools/list` and `tools/call`. A shape changes when a new observation says it
  should, not to make a test pass.
- Each adapter has its own tests; the Neovim one runs headless Neovim against a real server.

## Milestones

1. **M0, server skeleton.** WebSocket server, lock file, token, `initialize`, `tools/list`. A mock adapter.
2. **M1, read-only tools.** `getOpenEditors`, `getWorkspaceFolders`, `getCurrentSelection`,
   `getLatestSelection`, `getDiagnostics`; `selection_changed`, `at_mentioned`.
3. **M2, Neovim adapter.** Lua, over `vim.lsp.rpc`. Registers the main windows only.
4. **M3, actions.** `openFile`, `checkDocumentDirty`, `saveDocument`, `diagnostics_changed`.
5. **M4, diffs.** `openDiff`, `close_tab`, `closeAllDiffTabs`, and an accept/reject flow in the Neovim adapter.

## Open questions

- Whether to start the server on demand (systemd socket activation) or keep it running (issue #17).
- What an adapter should do when the server is not there: retry, and for how long (issue #10).
- Whether to write both a path and its symlink-resolved form into `workspaceFolders`, since the CLI compares text.
- A process id can be reused: if an unrelated process takes the id that a dead server recorded, its leftover lock
  files are not cleaned up until that process ends. Recording the start time of the process as well would settle it.
- A named-pipe transport for Windows.
