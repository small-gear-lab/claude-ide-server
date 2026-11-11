# Fixtures

What the other side is expected to say, written down from what was observed, as data.

- `claude_initialize.json`: the parameters of the `initialize` that Claude Code 2.1.322 sent to the probe
  (see `docs/DESIGN.md`, "What a real CLI sends").
- `*.shape.json`: the shape of something that the server sends or writes. A shape is compared with
  `common::shape::assert_shape`: an object in a shape lists the keys that must be there (others are allowed), a
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
- The shapes of the notifications and of the results of the read-only tools were **not** seen from a real Claude Code.
  They follow the public protocol notes of other implementations and the behaviour of the existing IDE integrations
  (`getCurrentSelection` as `{ success, text, filePath, selection }`, or `{ success: false, message }`; `getOpenEditors`
  as `{ tabs: [...] }`; `getDiagnostics` as a list of `{ uri, diagnostics }`; `selection_changed` with `fileUrl`;
  `at_mentioned` with 0-based lines). To be checked against a real CLI with a real adapter (issue #10), and corrected
  here if they differ. The diagnostics shape asks for `range.start` and `range.end`; the integrations seen send at
  least `range.start`.

A shape changes when a new observation says it should, not to make a test pass.
