# claude-ide-server

[日本語](README.ja.md)

An **unofficial** IDE integration server for [Claude Code](https://code.claude.com/docs/en/overview)'s `/ide`
protocol. It is not affiliated with or endorsed by Anthropic.

The protocol is not documented by Anthropic. What this project relies on comes from observing how the existing
integrations behave (the VS Code extension and the JetBrains plugin), and from the public write-up in
[coder/claudecode.nvim](https://github.com/coder/claudecode.nvim/blob/main/PROTOCOL.md). It can break when Claude
Code changes.

> **Status: early development.** The core library talks to a real Claude Code (the handshake, the list of tools, tool
> calls, notifications), and is tested with stand-ins for Claude Code and for an editor. The server runs as an
> executable (`claude-ide-server`, with an example systemd socket unit under `contrib/`). There is no editor adapter yet
> (#10).

## What it will be

Claude Code connects to an IDE over a WebSocket speaking a variant of MCP (JSON-RPC 2.0). The IDE writes a lock
file under `~/.claude/ide/`, and Claude Code finds it and connects. This project is the IDE side of that
connection, split into two parts:

```
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix socket)── an editor-side adapter
```

- **The server** speaks the Claude Code protocol: the WebSocket, the lock file and its token, and the MCP
  tools and notifications. It knows nothing about any particular editor.
- **An editor adapter** connects to the server, says which workspace it has, and answers the server's requests
  (open a file, report the selection, show a diff) over JSON-RPC. When the adapter disconnects, the server drops
  that workspace and its lock file, so a crashed editor leaves nothing behind.

The first adapter will be for Neovim, written in Lua.

## Documentation

- [docs/DESIGN.md](docs/DESIGN.md): the design, the protocol between the server and an editor adapter, and the milestones.

The documents are written in English and have a Japanese version (`README.ja.md`, `docs/ja/`), built from the
gettext catalogs in `docs/locale/ja/` by `tools/build-docs.sh`.

## Layout (planned)

| Where | What |
|---|---|
| `crates/core/` | the server: WebSocket, lock files, authentication, MCP handling |
| `editors/nvim/` | the Neovim adapter |
| `docs/` | the protocol notes and the design |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
