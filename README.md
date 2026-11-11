# claude-ide-server

<a href="README.ja.md">日本語</a>

An **unofficial** IDE integration server for [Claude Code](https://code.claude.com/docs/en/overview)'s `/ide`
protocol. It is not affiliated with or endorsed by Anthropic.

The protocol is not documented by Anthropic. What this project relies on comes from observing how the existing
integrations behave (the VS Code extension and the JetBrains plugin), and from the public write-up in
[coder/claudecode.nvim](https://github.com/coder/claudecode.nvim/blob/main/PROTOCOL.md). It can break when Claude
Code changes.

> **Status: early development, and not published.** The server implements the whole of the protocol: the handshake, every
> tool of the VS Code extension that makes sense for a terminal editor (opening files, the selection, diagnostics,
> diffs with accept and reject), and the notifications. It was tried with a real Claude Code (2.1.322), on Linux, with
> the Neovim adapter. It runs as an executable (`claude-ide-server`) on Linux (systemd socket activation), macOS (launchd)
> and Windows (a named pipe, and a Task Scheduler task); macOS and Windows are tried in CI, with stand-ins for Claude
> Code, not with a Claude Code of their own. Not done: Claude Code in WSL with an editor on Windows (#24), and
> publishing to crates.io (#16); there are no binaries to download, so you build it yourself.

## What it is

Claude Code connects to an IDE over a WebSocket speaking a variant of MCP (JSON-RPC 2.0). The IDE writes a lock
file under `~/.claude/ide/`, and Claude Code finds it and connects. This project is the IDE side of that
connection, split into two parts:

```
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix socket, or named pipe)── an editor-side adapter
```

- **The server** speaks the Claude Code protocol: the WebSocket, the lock file and its token, and the MCP
  tools and notifications. It knows nothing about any particular editor.
- **An editor adapter** connects to the server, says which workspace it has, and answers the server's requests
  (open a file, report the selection, show a diff) over JSON-RPC. When the adapter disconnects, the server drops
  that workspace and its lock file, so a crashed editor leaves nothing behind.

The first adapter is the one for Neovim, written in Lua (`editors/nvim`).

## Documentation

- [docs/DESIGN.md](docs/DESIGN.md): the design, the protocol between the server and an editor adapter, and the milestones.
- [docs/NVIM.md](docs/NVIM.md): the Neovim adapter: installing it, its options, and what it does.

The documents are written in English and have a Japanese version (`README.ja.md`, `docs/ja/`), built from the
gettext catalogs in `docs/locale/ja/` by `tools/build-docs.sh`.

## Layout

| Where | What |
|---|---|
| `crates/core/` | the server library: WebSocket, lock files, authentication, MCP handling, the adapter protocol |
| `crates/server/` | the executable, `claude-ide-server` |
| `crates/launchd/`, `crates/windows/` | the calls into macOS and Windows that need `unsafe`, kept apart from the rest |
| `crates/test-support/` | what the tests of the other crates share: stand-ins for Claude Code and for an adapter, and fixtures |
| `editors/nvim/` | the Neovim adapter |
| `contrib/` | files for systemd, launchd and Task Scheduler |
| `docs/` | the design, the adapter protocol, and the notes on what Claude Code does |
| `tools/` | the build of the Japanese documents |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md): how commits and the Japanese documents are handled, how to run the checks, and
why statements about Claude Code come with where they were learned.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
