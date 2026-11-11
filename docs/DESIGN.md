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

**(assumption)** `/ide` lists the lock files whose `workspaceFolders` contain the CLI's working directory, and
checks that `pid` is alive. The server writes the adapter's pid into the lock file for that reason.

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
  transport. **(assumption)** Neovim's `vim.lsp.rpc.connect` can connect to a Unix socket and be driven by hand;
  to be confirmed with a prototype.
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

## Hub: one entry for all workspaces (later)

By default each workspace is its own entry, as in the VS Code and JetBrains integrations: the lock file lists
only that workspace's folders, and every tool answers for that workspace only. A CLI whose working directory is
not inside any workspace (for example one started in a parent directory) finds no entry.

An optional **hub** adds one more entry that stands for all registered workspaces:

- Its lock file has its own port and token, an `ideName` that tells it apart (for example `Neovim (all)`), and
  the folders of every workspace in `workspaceFolders`. The per-workspace entries stay as they are.
- `getWorkspaceFolders` answers with all folders. `getOpenEditors` and `getDiagnostics` merge the answers of all
  adapters. `getCurrentSelection` answers with the workspace that was touched last.
- `openFile`, `saveDocument` and `checkDocumentDirty` go to the adapter whose workspace contains the path.
- The server needs no extra messages for this: it already holds every registration, and it fans a request out
  to the adapters. When the last adapter goes away, the hub entry goes with it.

Merging all folders into the per-workspace entries instead is not done: every CLI would then match every entry
**(assumption)**, and one project's Claude would see the paths of unrelated projects.

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
6. **M5, the hub** (optional). One extra entry for all workspaces; see above.

## Open questions

- How the CLI chooses among several lock files, and what it does with `pid` (assumed above).
- Whether to start the server on demand (systemd socket activation) or keep it running.
- What an adapter should do when the server is not there: retry, and for how long.
- Whether to accept lock files for workspaces in a path that is a symlink, and how to compare paths.
- Whether a hub entry confuses `/ide` when it and a per-workspace entry match the same directory.
- A named-pipe transport for Windows.
