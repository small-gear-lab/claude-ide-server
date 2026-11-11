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

A shape changes when a new observation says it should, not to make a test pass.
