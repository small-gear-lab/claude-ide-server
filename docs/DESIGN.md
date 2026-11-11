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
- Messages are JSON-RPC 2.0, based on MCP 2025-03-26.
- The CLI sends `initialize`, then the notification `ide_connected` with its pid.

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

`register` carries `protocolVersion`. The server refuses an adapter whose major version it does not know.
Methods can be added without a major bump; an adapter lists in `methods` what it implements.

## Structure of the core crate

The crate runs on tokio: its `async` functions must be called inside a tokio runtime, and tokio's I/O traits
(`AsyncRead`, `AsyncWrite`) are the common currency between layers. Other tokio types (channels, tasks) and the
types of the libraries below do not appear in the crate's public interface.

Each outside library sits behind a type of the crate's own, and what lies inside knows only those types. There are
three layers:

- **The Claude side:** the WebSocket server (`ws`), the MCP handling, the lock files.
- **The middle:** workspaces, the dispatch of tool calls, and the crate's own types for tools, selections and
  diagnostics.
- **The adapter side:** the adapter transport and the JSON-RPC framing.

Three seams are drawn on purpose, where this design already says that something may change:

1. **The adapter transport.** A Unix socket now, a named pipe later. Everything above it works with a duplex byte
   stream that a listener accepts, not with a Unix socket type.
2. **The MCP SDK.** If `rmcp` is used, its types stay inside the Claude side. The middle and the adapter side use
   the crate's own types, so that a new version of the SDK, or a different SDK, touches one layer.
3. **The file system and the environment.** The lock file directory, the runtime directory and the check that a
   `pid` is alive are passed in as configuration. Tests pass temporary directories.

Nothing else is put behind a trait in advance (TCP itself, the JSON shape of a lock file). A seam is added when
something is known to change, not before.

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
  handshake, authentication, every tool, notifications, diffs, disconnects and cleanup.
- The expected shapes of tool results come from the observed behaviour above, written down as fixtures.
- Each adapter has its own tests; the Neovim one runs headless Neovim against a real server.

## Milestones

1. **M0, server skeleton.** WebSocket server, lock file, token, `initialize`, `tools/list`. A mock adapter.
2. **M1, read-only tools.** `getOpenEditors`, `getWorkspaceFolders`, `getCurrentSelection`,
   `getLatestSelection`, `getDiagnostics`; `selection_changed`, `at_mentioned`.
3. **M2, Neovim adapter.** Lua, over `vim.lsp.rpc`. Registers the main windows only.
4. **M3, actions.** `openFile`, `checkDocumentDirty`, `saveDocument`, `diagnostics_changed`.
5. **M4, diffs.** `openDiff`, `close_tab`, `closeAllDiffTabs`, and an accept/reject flow in the Neovim adapter.

## Open questions

- Whether to start the server on demand (systemd socket activation) or keep it running.
- What an adapter should do when the server is not there: retry, and for how long.
- Whether to write both a path and its symlink-resolved form into `workspaceFolders`, since the CLI compares text.
- A named-pipe transport for Windows.
