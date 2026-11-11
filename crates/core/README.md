# claude-ide-core

The library behind [`claude-ide-server`](https://crates.io/crates/claude-ide-server): the side of the `/ide` protocol of
[Claude Code](https://code.claude.com/docs/en/overview) that does not depend on an editor. **Unofficial: it is not affiliated
with or endorsed by Anthropic**, and the protocol is not documented by Anthropic, so it can break when Claude Code changes.

It has three sides, which meet in `workspace`: the Claude side (the WebSocket server that Claude Code connects to, the lock
files that it finds, and MCP, on `rmcp`), the adapter side (an editor registers a workspace and answers requests, in JSON-RPC
with `Content-Length` framing as in LSP), and the middle (a port and a lock file for each registered adapter, and the tools).

It is for the executable `claude-ide-server`, and for someone who wants to write the server side of another kind. The design,
the messages between the server and an adapter, and what Claude Code was seen to do, are in the [repository](https://github.com/small-gear-lab/claude-ide-server) (`docs/DESIGN.md`).
The API is of a 0.x version, and changes between minor versions.

Licensed under either of the Apache License 2.0 or the MIT license, at your option.
