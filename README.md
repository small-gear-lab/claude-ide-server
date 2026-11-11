# claude-ide-server

An **unofficial** IDE integration server for [Claude Code](https://code.claude.com/docs/en/overview)'s `/ide`
protocol. It is not affiliated with or endorsed by Anthropic.

The protocol is not documented by Anthropic. What this project relies on comes from observing how the existing
integrations behave (the VS Code extension and the JetBrains plugin), and from the public write-up in
[coder/claudecode.nvim](https://github.com/coder/claudecode.nvim/blob/main/PROTOCOL.md). It can break when Claude
Code changes.

> **Status: design stage.** There is no working code yet.

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

## Layout (planned)

| Where | What |
|---|---|
| `crates/core/` | the server: WebSocket, lock files, authentication, MCP handling |
| `editors/nvim/` | the Neovim adapter |
| `docs/` | the protocol notes and the design |

## License

Not decided yet.
