# claude-ide-server

The server that lets the `/ide` of [Claude Code](https://code.claude.com/docs/en/overview) connect to an editor that has no
integration of its own. **Unofficial: it is not affiliated with or endorsed by Anthropic**, and the protocol that it speaks is
not documented by Anthropic, so it can break when Claude Code changes.

The server speaks the protocol of Claude Code (the WebSocket, the lock file and its token, the MCP tools and notifications)
and knows nothing about a particular editor. An *adapter* for each editor connects to it, says which workspace it has, and
answers its requests (open a file, report the selection, show a diff). The first adapter is the one for Neovim, in Lua.

    cargo install claude-ide-server

The executable is also on the [Releases](https://github.com/small-gear-lab/claude-ide-server/releases) page, built for Linux, macOS and Windows,
and so is the adapter for Neovim (`claude-ide-nvim-<version>.zip`), which goes with the server of the same version. How to
install and use them, and the design, are in the [repository](https://github.com/small-gear-lab/claude-ide-server).

The protocol between the server and an adapter, and the library that the server is made of, are in the crate
[`claude-ide-core`](https://crates.io/crates/claude-ide-core).

Licensed under either of the Apache License 2.0 or the MIT license, at your option.
