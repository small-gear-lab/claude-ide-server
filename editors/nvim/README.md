# claude-ide-nvim

The Neovim adapter of [claude-ide-server](https://github.com/small-gear-lab/claude-ide-server): it registers a Neovim with
the server, so that the `/ide` of Claude Code finds it. Unofficial, and not affiliated with Anthropic.

It needs Neovim 0.11 or newer, and the `claude-ide-server` executable of the same release, on the `PATH` (or named in the
`server.command` option).

Unpack this directory anywhere, put it on the runtime path, and call `setup`:

```lua
vim.opt.runtimepath:append('/path/to/claude-ide-nvim-<version>')
require('claude_ide').setup()
```

The options, the commands and what the adapter does are in
[docs/NVIM.md](https://github.com/small-gear-lab/claude-ide-server/blob/main/docs/NVIM.md) (Japanese:
[docs/ja/NVIM.md](https://github.com/small-gear-lab/claude-ide-server/blob/main/docs/ja/NVIM.md)).
