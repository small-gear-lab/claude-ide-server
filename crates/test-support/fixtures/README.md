# Fixtures

What the other side is expected to say, written down from what was observed, as data.

- `claude_initialize.json`: the parameters of the `initialize` that Claude Code 2.1.322 sent to the probe
  (see `docs/DESIGN.md`, "What a real CLI sends").
- `*.shape.json`: the shape of something that the server sends or writes. A shape is compared with
  `shape::assert_shape`: an object in a shape lists the keys that must be there (others are allowed), a
  one-element array means that every element has that shape, a string like `"<string>"` stands for any value of that
  type (`<string>`, `<number>`, `<bool>`, `<object>`, `<array>`, `<any>`), and anything else must be equal.
  - `lock_file.shape.json`: a lock file, as the IDE integrations write it.
  - `initialize_result.shape.json`, `tools_list_result.shape.json`, `tool_call_result.shape.json`: the results of the
    MCP requests that Claude Code makes.

  - `selection_changed.shape.json`, `at_mentioned.shape.json`, `get_selection_result.shape.json`,
    `get_selection_none_result.shape.json`, `get_open_editors_result.shape.json`, `get_diagnostics_result.shape.json`:
    what the server sends for the selection, a mentioned file, and the results of `getCurrentSelection` /
    `getLatestSelection`, `getOpenEditors` and `getDiagnostics`.

## Where each shape comes from

- The handshake, the lock file: seen with a real Claude Code 2.1.322 (the handshake) and with the existing IDE
  integrations (the lock file).
- Seen with a real Claude Code 2.1.322, driven in a terminal against the Neovim adapter (issue #20):
  - `selection_changed` (with `fileUrl`, and `isEmpty`): taken, shown as `Selected N lines from <file> in Neovim`, and
    known to the model; a cursor without a selection tells the model which file is open.
  - `at_mentioned` (0-based lines): put into the prompt as `@file#L2-4` (the lines that the CLI shows are 1-based).
  - The result of `getDiagnostics` as a list of `{ uri, diagnostics: [ { message, severity, range, source } ] }`: taken,
    called with and without a `uri`; the model reported the message, the severity, the source and the range, with the
    columns in UTF-16 code units.
  - A notification that arrives right after the handshake is thrown away (see `docs/DESIGN.md`, "What Claude Code sends on its own").
- Read from the code of the VS Code extension (2.1.322), which is the IDE side that this server stands in for:
  `get_selection_result.shape.json` (with `success` and `fileUrl`), `get_latest_selection_result.shape.json` (without
  `success`), `get_open_editors_result.shape.json` (the five that every client takes; the extension adds more to a tab),
  and `diagnostics_changed.shape.json` (`{ uris }`). The texts of `openFile`, `checkDocumentDirty` and `saveDocument`
  and of the workspace folders are in `tests/actions.rs` and `src/tools.rs`, from the same code.
- **Not** seen from a real Claude Code: the results of `getCurrentSelection`, `getLatestSelection` and `getOpenEditors`. The CLI 2.1.322 does not
  call them at all (nor `checkDocumentDirty`, `saveDocument` and `getWorkspaceFolders`), so they cannot be checked
  against it; they follow the public protocol notes of other implementations and the behaviour of the existing IDE
  integrations (`getCurrentSelection` as `{ success, text, filePath, selection }`, or `{ success: false, message }`;
  `getOpenEditors` as `{ tabs: [...] }`). The diagnostics shape asks for `range.start` and `range.end`; the integrations
  seen send at least `range.start`.
- What the CLI sends to the editor on its own, from its code: `closeAllDiffTabs`, `openDiff`, `close_tab`,
  `getDiagnostics`, `set_permission_mode` and the notification `ide_connected`. It does not take `diagnostics_changed`.

A shape changes when a new observation says it should, not to make a test pass.
