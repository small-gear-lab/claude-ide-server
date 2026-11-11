# Design

Status: implemented, except for what the "Non-goals" and the "Open questions" below leave out. Statements about
Claude Code's behaviour come from observing Claude Code (2.1.322), from reading its code, and from reading existing
integrations and public write-ups; each says which, and they are marked **(assumption)** where they have not been
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
- WSL: Claude Code in Linux, under WSL, with an editor on Windows (issue #24). The server and the Neovim adapter run on Windows itself (see "Running the server").
- Replacing the official integrations. This is for editors that do not have one.
- A hub entry that stands for all workspaces. Considered and left out: a CLI normally runs inside the project that its editor has open, and a second entry that matches the same directory would make `/ide` ambiguous.

## Overview

```
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix socket, or named pipe)── editor adapter
   (1 per CLI)                    (1 process)                                    (1 per workspace)
```

The server has two faces:

- **Claude side.** For each registered workspace, it listens on a loopback WebSocket port, writes a lock file,
  authenticates the CLI with a token, and speaks MCP (JSON-RPC 2.0): it answers `initialize`, `tools/list` and
  `tools/call`, and sends notifications.
- **Adapter side.** It listens on one Unix socket (a named pipe on Windows). An adapter connects, registers a workspace, pushes
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
| `openDiff` | `openDiff` | blocks until the user accepts or rejects; answers `FILE_SAVED` and the contents, or `DIFF_REJECTED` and the tab name |
| `close_tab` | `closeDiff` | by `tab_name`, the one given to `openDiff`; answers `TAB_CLOSED`, also if there is no such tab or no adapter method |
| `closeAllDiffTabs` | `closeAllDiffs` | answers `CLOSED_<n>_DIFF_TABS`; `CLOSED_0_DIFF_TABS` if the adapter has none |
| `set_permission_mode` | `setPermissionMode` | `{ mode }`; answers `OK` if the adapter has none |
| `getOpenEditors` | `getOpenEditors` | |
| `getCurrentSelection` | `getCurrentSelection` | |
| `getLatestSelection` | `getLatestSelection` | the last selection, even if the editor lost focus |
| `getWorkspaceFolders` | (answered by the server) | from the registration |
| `getDiagnostics` | `getDiagnostics` | one URI, or all |
| `checkDocumentDirty` | `checkDocumentDirty` | |
| `saveDocument` | `saveDocument` | |

`executeCode` (Jupyter only) is out of scope.

`closeAllDiffTabs` and `set_permission_mode` are called by Claude Code itself (see "What Claude Code sends on its own"),
whether or not they were listed, so they are answered even when the adapter has no method for them; they are listed
only when it has.

How a call turns out:

- The answer to a tool call is a text content, and for `openDiff` two (`FILE_SAVED` and then the contents of the file).
  For the tools whose answer the server does not turn into a text of the VS Code extension (only `set_permission_mode`
  is left), a string from the adapter is the text as it is; anything else is written as JSON.
- A call that fails (the adapter answers with an error, does not answer in time, or is gone) is still an answer: the
  result carries `isError: true` and the reason as its text. Claude gets to see why.
- The arguments are checked against the schema of the tool before the adapter is asked: a required argument that is
  missing, or one of the wrong type, fails the call (`invalid arguments for openFile: filePath is required`), and
  the adapter is not asked. What the model sends is not trusted to match the schema that it was given.
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

The VS Code extension sends `diagnostics_changed` as it sees diagnostics change (one notification for each change
event, with the URIs of the files). Claude Code 2.1.322 does not take it (its code does not mention it): it asks for the
diagnostics itself, with `getDiagnostics`, around its own edits. Other clients may take it, so it is sent.

### What Claude Code sends on its own

From the code of Claude Code 2.1.322 (a reading of the program, not a specification), and checked in a terminal against
the Neovim adapter where it says so. These are not answers to anything that the editor did.

- The notification `ide_connected`, right after the handshake (with the process of Claude Code).
- Tool calls to the editor: `closeAllDiffTabs` (right after it connects, and when it closes diffs), `openDiff` and
  `close_tab` (diffs), `getDiagnostics` (for the file that it is about to edit, before and after the edit, and for all
  files; it drops the result, with a `Diagnostics file path mismatch`, if the `uri` that comes back is not the file that
  it edited, after normalising both), and `set_permission_mode` (`{ mode: "ask" | "skip_all_permission_checks" }`,
  whenever its permission mode changes).
- **The `uri` that it asks for is the one that it wants back.** It normalises the two (it takes off `file://` and the
  like) and does not resolve symbolic links, so a file that it names through a link is not the file that comes back
  under its real path. An adapter finds the file by its real path, and answers under the `uri` that it was asked for
  (checked with the Neovim adapter, in a workspace that is a link: the `uri` that was asked for, through the link, came
  back with the diagnostics of the file). What was not checked is the call that Claude Code makes by itself around an
  edit, where the `Diagnostics file path mismatch` would show.
- It takes these notifications from the editor: `selection_changed`, `at_mentioned`, `log_event` and the MCP logging
  message. It does not use `getOpenEditors`, `getCurrentSelection`, `getLatestSelection`, `checkDocumentDirty`,
  `saveDocument` and `getWorkspaceFolders`.
- **A notification that arrives right after the handshake is thrown away.** Claude Code sets up what receives
  notifications some time after it is connected, and that time varies (checked: a `selection_changed` sent at once never
  arrives; sent after 25 ms or 50 ms it did in three runs of three, after 100 ms and 150 ms in two of three). The server
  cannot tell whether a notification was taken: a notification has no answer, and the MCP specification (2025-11-25,
  "Lifecycle") has no signal for when a client can take them (the first `tools/list` is not one either). So the state
  that is remembered (the selection) is sent again at 250 ms, 500 ms, 1 s and 3 s after the handshake, in addition to the
  replay at once (`claude_state_resends`). 500 ms is the time at which the VS Code extension sends the selection, once,
  after a connection (read from its code). With 250 ms, 1 s and 3 s (the times that were measured), ten runs out of ten
  reached the model on an idle machine. With the CPU busy as well (`stress-ng` with 32 workers on 16 cores, a load
  average of 28 to 42), twelve runs out of twelve did, with a selection made in Neovim before Claude Code started and with
  500 ms among the times. Claude Code was slow to start there: it connected 10 to 21 s after the adapter had registered
  (2.4 s on an idle machine). The log of the server does not say which of the resends a run was served by, so these runs
  do not say that any of the times can be left out; the times were not changed. Later notifications are taken, so this
  is only about the first seconds after a connection.

### What the VS Code extension does

The VS Code extension is the IDE side that this server stands in for, so its code (2.1.322, read, not specified) is the
reference for what an editor answers. What a client of this protocol may rely on:

- It serves one client at a time: a new connection closes the one before it. This server keeps several (see "Several
  CLIs on one workspace").
- **After a connection it waits 500 ms, and then sends the selection that was last reported** (`selection_changed`), and
  the telemetry events that were queued (`log_event`, which this server has no use for). The wait is there because a
  client that has just connected is not ready for a notification (see "What Claude Code sends on its own").
- It sends `diagnostics_changed` for every change of diagnostics, with the URIs of the files.
- A tool that takes a `filePath` takes a relative one too, from the first folder of the workspace; the results are text
  that is JSON written with two spaces of indent, and the messages are the ones in the tables above and below.
- `openFile` looks for `startText` with a plain search from the start of the document, and `endText` after it; a
  document is counted as having a line more than there are, after a final newline.
- Its own tracking of diagnostics around the edits of Claude Code (it waits for the editor to publish them) is done
  inside the extension, not through this protocol: for a file that was edited, it waits up to 750 ms for the first
  update, then until 150 ms pass with none, and in all no longer than 1500 ms (1000 ms for a file that no editor shows). An
  adapter that has no such wait answers with what it had before the edit; the Neovim adapter waits, with the same times
  (see "Diagnostics" in [NVIM](NVIM.md)).

### Slow readers

A client that does not read what it is sent must not hold up the other clients, or the workspace. Notifications to
Claude Code are put in a queue of the client's own and sent from it by a task of its own; sending never waits.

- A notification that is a **state** (the selection) replaces the notification of the same method that is still
  waiting, so that a slow client is given the current state, not the history of it, and the latest one is never the one
  that is lost.
- The others (a mentioned file) are kept in order. If more than 64 are waiting for a client, the oldest is dropped.
- The events that the owner of a workspace reads (`Workspace::next_event`) are information, and are handed over the
  same way: past 256 waiting, the newest are dropped and counted (`dropped_events`). Passing the selection on to Claude
  Code does not depend on anyone reading them.

### Several CLIs on one workspace

Each WebSocket connection is a separate client. The server accepts more than one per workspace (the VS Code
extension keeps only the latest). A diff belongs to the connection that opened it, and is closed when that
connection closes it or goes away.

## The adapter side

### Transport

- A Unix domain socket at `$XDG_RUNTIME_DIR/claude-ide-server/adapter.sock` (without `XDG_RUNTIME_DIR`, as on macOS,
  `claude-ide-server-<uid>/adapter.sock` under the temporary directory), in a directory of mode 0700. The socket file is
  mode 0600. On Windows it is a named pipe instead (see "Where the adapter socket comes from").
- JSON-RPC 2.0 with `Content-Length` framing, as in LSP. Editors that already have an LSP client can reuse its
  transport. Checked with Neovim 0.12: `vim.lsp.rpc.connect(path)` connects to a Unix socket, the adapter can
  send requests and notifications and answer the server's requests (the `server_request` dispatcher), and when
  the server closes the connection `on_exit` is called and `is_closing()` becomes true. A connection that fails
  raises no error either: Neovim prints a warning and no `on_exit` follows, so the adapter first tries a connection of
  its own and closes it (the server ignores a connection that says nothing), and connects with `vim.lsp.rpc` only if
  that worked. That also lets it retry without a message. See [the Neovim adapter](NVIM.md).
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
| `openFile` | request | `filePath`, `preview`, `startText`, `endText`, `selectToEndOfLine`, `makeFrontmost` → `{ filePath, languageId, lineCount, isDirty, isUntitled, selection? }` |
| `getOpenEditors` | request | → `{ tabs: [{ uri, isActive, label, languageId, isDirty, ... }] }` |
| `getCurrentSelection` | request | → selection, or `{ success: false }` |
| `getLatestSelection` | request | → selection, or `{ success: false }` |
| `getDiagnostics` | request | `uri?` → `[{ uri, diagnostics: [{ message, severity, range, source }] }]` |
| `checkDocumentDirty` | request | `filePath` → `{ isDirty, isUntitled }`, or `{ success: false }` if the document is not open |
| `saveDocument` | request | `filePath` → `{ saved }`, or `{ success: false }` if the document is not open |
| `openDiff` | request | `oldFilePath`, `newFilePath`, `newFileContents`, `tabName`, `diffId` → `{ accepted: true, contents }` or `{ accepted: false }`, answered when the user decides |
| `closeDiff` | request | `tabName`, and `diffId` if only that diff is to be closed → anything |
| `closeAllDiffs` | request | → `{ closed: n }` |
| `shutdown` | notification | the server is stopping |

A position is `{ line, character }`, with `line` 0-based and `character` in UTF-16 code units (the LSP convention),
and the end of a range is exclusive. The adapter converts from the editor's own unit. **(assumption)** The CLI only
counts lines from a selection, so a mistake in `character` does not change what Claude sees.

### What an adapter reports of the editor

What an adapter sends is checked by the server. A missing field or a wrong type is not guessed at: in the answer to a
request it makes the tool call fail, with the place of the fault in the text (for example
`the editor's answer to getOpenEditors is not valid: $.tabs[1].isDirty: expected a boolean`), and in a notification
it means that the notification is not passed on and the owner of the workspace is told (`MalformedNotification`).

- A **selection** is `{ filePath, text, selection: { start, end } }`, where `start` and `end` are positions. It is the
  parameters of `selectionChanged`, and the answer to `getCurrentSelection` and `getLatestSelection` when there is
  one; when there is none, the answer is `{ "success": false, "message": ... }` (the message is optional). A cursor
  without a selection is a selection with an empty `text` and `start` equal to `end`. The server works out `isEmpty`
  from the range.
- **`atMention`** is `{ filePath, lineStart, lineEnd }`, 0-based, `lineEnd` being the last line.
- A **`filePath`** is an absolute path (on Unix it starts with `/`; on Windows with a drive and a separator, `C:\Users\u\a.lua`,
  or with a separator, as Node, which Claude Code is, takes it: `path.win32.isAbsolute`). Claude Code works out what to show
  from it, and a relative one would be taken relative to the directory that Claude Code runs in, which is not that of the
  editor.
- **Paths on Windows** are native paths, written out in full. An editor may know a file by the short form of 8.3
  (`C:\Users\RUNNER~1\a.lua`), and Claude Code compares a path with its own working directory as text, which has the long
  form: the Neovim adapter reports the long form (the real path, in its own case). Otherwise the server does not change what
  an editor reports (the separators and the case of the drive letter stay as they are). The `fileUrl` that the server makes
  from a `filePath` is `file:///C:/Users/u/a%20b.lua`, and `file://server/share/a.lua` for a path of a share; a `uri` that an
  adapter makes is its own (Neovim's `vim.uri_from_fname` writes the same). A relative `filePath` in a tool call is
  joined with the first folder of the workspace, with the separator that the folder uses, and no `..` goes above the drive
  or the share (`crates/core/src/paths.rs`, which works on strings and is tested for both ways of writing a path on every
  system).
- **The answer to `getOpenEditors`** is `{ tabs: [ ... ] }`. A tab has five things that every client of this protocol
  takes, and all five are required: `uri`, `isActive`, `label`, `languageId`, `isDirty`. The VS Code extension adds more
  to a tab, and an adapter may leave any of them out: `isPinned`, `isPreview`, `groupIndex`, `viewColumn` (the group of
  tabs that it is in, and its column), `isGroupActive`, `fileName`, `lineCount`, `isUntitled`, and for the active tab
  `selection: { start, end, isReversed }`. They are passed on as they are, and checked when they are there.
- **The answer to `openFile`** is `{ filePath, languageId, lineCount, isDirty, isUntitled }`, and `selection` if
  `startText` was given: what came of looking for the text, one of `"text"` (the text of `startText` is selected),
  `"range"` (from `startText` to `endText`), `"positioned"` (`startText` was found and the cursor is there, `endText`
  was not) and `"not_found"`. The server writes the result as the VS Code extension does: the message alone
  (`Opened file and selected text "..."`, and so on) when `startText` was given, `Opened file: <path>` when the file is
  made the frontmost one (`makeFrontmost`, true unless it says false), and otherwise the details as JSON. A file that
  cannot be opened is an error of the adapter (`File not found: <path>`), which makes the tool call fail. A path that
  is not absolute is made absolute from the first folder of the workspace before the adapter is asked, as the VS Code
  extension does, so the adapter gets an absolute one.
- **The answers to `checkDocumentDirty` and `saveDocument`** are `{ isDirty, isUntitled }` and `{ saved }`, or
  `{ "success": false }` for a document that is not open (the server says `Document not open: <path>`). `saved` is
  false if there was nothing to write or writing failed.
- **`diagnosticsChanged`** is `{ uris: [ ... ] }`: the URIs of the files whose diagnostics changed. It is an event: each
  names its files, none replaces another, and a Claude Code that connects later is not told of it.
- **The answer to `getDiagnostics`** (with `{ uri }` as the parameters, or none for all files) is
  `[ { uri, diagnostics: [ { message, severity, range: { start, end }, source? } ] } ]`. `severity` is one of
  `"Error"`, `"Warning"`, `"Information"` and `"Hint"`; an empty list is an answer. For a `uri` that was asked for, the
  `uri` of the entry is that one, as it was spelt (see "What Claude Code sends on its own"), and the answer is the
  diagnostics of the file as it is now: an adapter should wait, for a short time, for those of a file that was just
  changed under the editor.

The server passes these on to Claude Code in the shapes that it expects (see the notifications above): `selectionChanged`
becomes `selection_changed` (with a `fileUrl` added, and `isEmpty`), `atMention` becomes `at_mentioned`,
`diagnosticsChanged` becomes `diagnostics_changed`, and the answers become the text of the tool results, in the shapes
that the VS Code extension writes (for example, `getCurrentSelection` has `success` and `getLatestSelection` has not). A Claude Code that connects later is sent the `selection_changed` that is current
(the last one), so that it starts from the selection that the user has now; it is sent again a few times after the
handshake, because the first may be thrown away (see "What Claude Code sends on its own").

**Diffs.** The editor does not write the file: Claude Code does, with the contents that the editor gives back (the VS Code
extension answers `FILE_SAVED` and then the contents of the file as they are when the user accepts, which may have been
edited in the diff; Claude Code reads that second text and uses it, and takes anything else than `FILE_SAVED` as not
accepted). So:

- `openDiff` is answered when the user decides, and until then it is not answered: the server does not put a time limit on
  it (the adapter's other requests have one), and nothing else waits for it. The answer is `{ accepted: true, contents }`
  (without `contents` it is refused: Claude Code could not write the file) or `{ accepted: false }`. The server turns them
  into `FILE_SAVED` and the contents, and `DIFF_REJECTED` and the name of the tab.
- The server gives each diff a number, `diffId`. A new `openDiff` of the name of a diff that is shown replaces that diff: the
  adapter ends the older one, rejected. If the Claude Code that asked for a diff cancels the call or goes away, the server
  sends `closeDiff` with the `tabName` and that `diffId`, so that the diff does not stay on the screen with nobody to ask,
  and the adapter closes it only if it is the diff that is shown under that name (a newer one is left alone). `close_tab`
  from Claude Code has no `diffId`: it closes whichever diff has the name.
- `closeAllDiffs` is answered with how many diffs it closed (`{ closed: n }`).

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

1. **The adapter transport.** A Unix socket, or a named pipe on Windows. Everything above it works with a duplex byte
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

## Running the server

`claude-ide-server` (the `crates/server` crate) is one executable for every editor. `claude-ide-server --help` lists
its switches: `--socket` (where the adapters connect), `--idle-timeout` (exit when the last adapter has been gone for
that many seconds), `-v` / `-q` (the log). The log goes to standard error, through `tracing`; `CLAUDE_IDE_SERVER_LOG`
or `RUST_LOG` replace what the switches say. It runs until `SIGINT` or `SIGTERM` (on Windows, Ctrl-C or Ctrl-Break, the closing of the console, or the shutdown of the system), then tells every adapter `shutdown`
and removes the lock files and ports, and the socket if it made it. Exit codes: 0 stopped as asked, 1 could not start
(or the socket closed), 2 wrong command line, 3 the workspaces were not closed in time.

### Where the adapter socket comes from

The server does not care how it was started: by hand, by a supervisor that listens for it and starts it at the first
connection (socket activation), or by an adapter that found no server. The one thing that differs is how the listening
socket is obtained, and that is hidden behind a small trait, `SocketSource` (`crates/server/src/activation.rs`). The
first source that applies is used:

| source | for | how |
|---|---|---|
| handed over | systemd, and other supervisors and container runtimes that follow the same convention | the socket is already listening on an inherited file descriptor (`LISTEN_PID`, `LISTEN_FDS`) |
| handed over by launchd | launchd on macOS | the socket is asked for by name, `adapter`, from the property list of the job (`launch_activate_socket`) |
| bound | everything else, and the default | the server makes the socket itself, mode 0600 in a 0700 directory (on Windows, a named pipe that only the user can open) |

A handed-over socket is not the server's to delete: it stays when the server exits, and the supervisor starts the
server again at the next connection. With `--idle-timeout` an on-demand server does not stay for ever. `contrib/` has
the files for each system, and each was tried with the real thing: `contrib/systemd` with a real systemd (the service
stays inactive until an adapter connects, and ends after the idle time while the socket stays), and `contrib/launchd`
on the macOS runner of CI, where `crates/server/tests/launchd.rs` loads the property list that is shipped into the domain
of the user (`gui/<uid>`; the runner has one) and checks the same four things: nothing runs after it is loaded, the first
connection starts the server and it serves the socket that launchd handed over, it ends after the idle time with the
socket still there, and the next connection starts it again.

**Where the adapters look for it.** By default each of them works out the same place as the server does. The place can also
be set once, for the server and for the adapters that read it, with `CLAUDE_IDE_SERVER_SOCKET` (`--socket` wins over it for
the server, and the `socket` option for the Neovim adapter): a supervisor, or a test that must not share the default, sets it.
`claude-ide-server --print-socket` prints the place (what `--socket` or the environment says, or else the default) and ends,
for an adapter that cannot work the default out: Neovim on Windows, where the name of the pipe has the security identifier of
the user in it, which Neovim does not give. That adapter asks the executable that `server.command` names, once.

What a system does differently is absorbed in a source of its own, selected by `cfg` where the system decides it:

- **systemd** (Linux): the handed-over source.
- **launchd** (macOS): launchd's socket activation hands a socket over by name, through `launch_activate_socket`, and not
  through the environment. The source asks for the socket named `adapter`; it does not apply when launchd did not start
  the process (the server was run by hand), or started it from a property list that has no such socket, and the server
  then makes its own. The call is C, so it needs `unsafe`, which the workspace forbids: it is in a crate of its own,
  `claude-ide-launchd`, the one place that has it (each use with its reason written down), and it gives back owned file
  descriptors, so the rest stays free of it. `SockPathName` in the property list cannot use `$TMPDIR` or `~`, so the
  file in `contrib/launchd` has `@SOCKET@` in its place, which the instructions in the file fill in with the path that
  the adapters look for by default (and make its directory: launchd does not). A machine with no domain of launchd that
  a user agent can be loaded into (a session with nobody logged in, a container) cannot run the test; it says so and ends,
  unless `CLAUDE_IDE_REQUIRE_LAUNCHD=1` is set, as CI does.
- **Task Scheduler** (Windows): a task cannot be started by a connection, so there is no socket activation: the server
  runs from log-on on (`contrib/windows`), and the adapters connect to a named pipe, `\\.\pipe\claude-ide-server-<sid>`
  (`--socket` takes another name). The pipe is made by `PipeListener` in the core crate, in place of the Unix socket, and
  the rest of the server does not know the difference. What is particular to it:
  - The name has the security identifier (SID) of the user in it, since the names of pipes are shared by everyone on the
    machine, and the pipe has an access list with the user alone and refuses clients from other machines. The first
    instance of the name is made as the first, which the system refuses if the name is taken, so a second server on the
    same name fails to start (exit code 1), as on Unix. A pipe goes with its last handle, so a server that died leaves
    nothing to be replaced.
  - The calls that need `unsafe` (whether a process is alive, who the user is, making the pipe with its access list) are
    in a crate of their own, `claude-ide-windows`, like the one for launchd.
  - The lock files are in `CLAUDE_CONFIG_DIR`, or else `%USERPROFILE%\.claude\ide` (`USERPROFILE` is where Node, which
    Claude Code is, takes the home directory from on Windows). There are no modes: a file has what the directory of the
    user gives it.
  - **Task Scheduler ends a task by killing the process**: on the runner the last result of a task that was stopped was
    `267014` (terminated by the user), and the server got no chance to clean up. So each lock file, and its record, is
    held by a handle that says "delete when closed", which the system closes when the process is gone, however it
    ended. (A reader, such as Claude Code, is not held back by it, and the file can still be replaced, which is how it
    is rewritten. What is left by a power cut, or on a system that does not do this, is removed by the next server, as
    on Unix.)
  - The console events of a server that is run by hand: Ctrl-C and Ctrl-Break are "interrupted", the closing of the
    console and the shutdown of the system are "terminated", and both end the server as `SIGINT` and `SIGTERM` do.
  - `contrib/windows/claude-ide-server.xml` is the task. Its declaration says UTF-16, as Task Scheduler writes it,
    because the `schtasks` program reads only that; the instructions in the file use PowerShell, which is given the text
    (`Register-ScheduledTask -Xml`). The task has no environment, so the server has the paths of the user.
  - Tried on the Windows runner of CI: all the tests of the core, and of the executable (an adapter registers over the
    pipe, a stand-in for Claude Code connects with the token from the lock file and calls a tool, a server that is killed
    leaves no lock file, a second server is refused, the idle exit), and `crates/server/tests/task_scheduler.rs`, which
    registers the file that is shipped, starts the task, does the same with the adapter and the stand-in, stops the task,
    and finds the pipe gone and no lock file left. That test runs only with `CLAUDE_IDE_TRY_TASK_SCHEDULER=1` (the task has
    the paths of the user, which a test must not take from a machine that has a server of its own); CI sets it.
  - What Claude Code does with the lock file on Windows, from its code (2.1.322), not tried with a Claude Code on Windows:
    it lists `*.lock` in `<config dir>/ide`, asks whether the process of `pid` is alive (`process.kill(pid, 0)`), and
    compares its working directory (the drive letter in upper case, in NFC) with each of `workspaceFolders` by
    containment. It connects to `127.0.0.1`; another address is for WSL with an editor on Windows (`runningInWindows`).
  - The Neovim adapter was run on the Windows runner (Neovim 0.12.6, in the tests of `crates/server/tests/nvim.rs`, which are
    for every system now): it registers over the pipe, follows `:cd`, reports the selection, the open editors and the
    diagnostics, opens, checks and saves files, shows the changes that Claude Code proposes and takes the user's decision, and
    starts the server detached and finds it again after it restarts. See "Windows" in [NVIM](NVIM.md).
  - Not done: starting the task on demand from an adapter (the adapter starts the executable itself, detached; it does not
    trigger the task), and WSL, which is issue #24.

An adapter that finds nobody listening starts this same executable itself (the Neovim adapter does, unless told not to):
connecting is how the server is activated, whether a supervisor or the adapter answers it. The server is then not a
child of the editor, and with `--idle-timeout` it ends when the last adapter has gone. Two adapters that do it at the
same moment are safe, because a second server on the same socket refuses to start (exit code 1).

## Security

- Every listener is on `127.0.0.1`. The adapter socket is in a 0700 directory and is itself 0600 (on Windows the pipe has an access list with the user alone, and refuses clients from other machines).
- The token is checked on every WebSocket upgrade. A web page cannot set that header, so a browser cannot talk
  to the port.
- Frame and message sizes are capped. A malformed message closes that connection and nothing else.
- On Windows the name of a pipe is shared by everyone on the machine. The access list keeps other users out of the pipe of the
  server, but a user who makes a pipe of that name first leaves the server unable to start (it says that another server is
  listening), and can get what an adapter sends to it: the folders of the workspace, the selection. Nothing is done about
  it yet: an adapter in Lua cannot see who owns a pipe. It matters on a machine that more than one user uses at the same time.
- Paths from Claude are used as given by the adapter; the server never reads or writes project files itself.
  Writing a file after an accepted diff is the adapter's job.

## Testing

- A mock Claude client (WebSocket) and a mock adapter (Unix socket) run in the same test process. Tests cover the
  handshake, authentication, every tool, notifications, diffs, disconnects and cleanup. The parts that the tests
  share are in a crate of their own, `crates/test-support`, which the tests of the other crates use: the adapter's end of a
  connection (`Client`), a workspace server on temporary directories (`Fixture`), Claude Code's client (`MockClaude`), and a
  workspace with both already connected (`Scene`). Most tests run over in-memory streams; one runs the whole path on a real Unix socket
  (`tests/end_to_end.rs`: register, connect with the token from the lock file, list the tools, call one, disconnect,
  clean up).
- The expected shapes of what the server sends and writes come from the observed behaviour above, written down as
  fixtures in `crates/test-support/fixtures/`: the `initialize` that Claude Code sends, and the shapes of a lock file and
  of the results of `initialize`, `tools/list` and `tools/call`. A shape changes when a new observation says it
  should, not to make a test pass.
- Each adapter has its own tests; the Neovim one runs headless Neovim against a real server.

## Milestones

All five are done; they are kept as the order in which it was built.

1. **M0, server skeleton.** WebSocket server, lock file, token, `initialize`, `tools/list`. A mock adapter.
2. **M1, read-only tools.** `getOpenEditors`, `getWorkspaceFolders`, `getCurrentSelection`,
   `getLatestSelection`, `getDiagnostics`; `selection_changed`, `at_mentioned`.
3. **M2, Neovim adapter.** The server executable (`crates/server`), and a Lua adapter, over `vim.lsp.rpc`. Registers the main windows only.
4. **M3, actions.** `openFile`, `checkDocumentDirty`, `saveDocument`, `diagnostics_changed`.
5. **M4, diffs.** `openDiff`, `close_tab`, `closeAllDiffTabs`, and an accept/reject flow in the Neovim adapter, with a provider that
   shows the diff and can be replaced.

## Open questions

- WSL, where Claude Code and the editor are on the two sides of the boundary between Linux and Windows (issue #24).
- Whether to write both a path and its symlink-resolved form into `workspaceFolders`, since the CLI compares text.
- A process id can be reused: if an unrelated process takes the id that a dead server recorded, its leftover lock
  files are not cleaned up until that process ends. Recording the start time of the process as well would settle it.
