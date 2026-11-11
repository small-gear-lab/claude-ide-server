# The Neovim adapter

The Neovim side of the connection: it registers a Neovim with `claude-ide-server`, so that Claude Code's `/ide`
finds it. The adapter is the directory `editors/nvim/` of this repository, written in Lua.

**What works now:** connecting to the server (and again when it is not there or goes away), registering the workspace,
following `:cd`, and sending the selection. **What does not yet:** answering Claude Code's requests for the open
editors, the selection and the diagnostics (issue #20), `openFile`, `checkDocumentDirty` and `saveDocument` (#11),
diagnostics notifications (#12), and diffs (#13, #14). Until #20 is done, Claude Code connects but most of its tools
fail.

## Requirements

- Neovim 0.11 or newer. CI runs the tests with 0.11.7 and 0.12.6, and 0.11.0 (the first of the 0.11 series) was tried by
  hand. Neovim 0.10 does not work (`vim.str_utfindex` takes other arguments there; nothing was tried beyond that).
- `claude-ide-server` on the `PATH` (see "Running the server" in [DESIGN](DESIGN.md)), or a supervisor that starts it at
  the first connection. The adapter starts the executable itself if nobody is listening.

## Installing

Add `editors/nvim` of a clone of this repository to the runtime path of Neovim, in whatever way your plugin manager
does that, and call `setup`:

```lua
vim.opt.runtimepath:append('/path/to/claude-ide-server/editors/nvim')
require('claude_ide').setup()
```

`setup` connects once Neovim has started. `setup({ autostart = false })` does not; `:ClaudeIde start` does.

## Options

```lua
require('claude_ide').setup({
  -- The server's adapter socket. By default, where the server puts it: $XDG_RUNTIME_DIR/claude-ide-server/adapter.sock.
  socket = nil,
  -- The name that /ide shows.
  ide_name = 'Neovim',
  -- Whether this Neovim is to register. See "Main windows only".
  should_register = function() return true end,
  -- The folders of the workspace. By default, the working directory. It is asked at registration, and when :cd changes it.
  workspace_folders = function() return { vim.uv.cwd() } end,
  -- The wait between attempts to connect: it doubles, up to the maximum.
  reconnect = { initial_ms = 500, max_ms = 10000 },
  -- What to do when nobody is listening on the socket. See "How it connects".
  server = { autostart = true, command = 'claude-ide-server', args = { '--idle-timeout', '300' } },
  -- How long the server has to answer `register`.
  register_timeout_ms = 5000,
  -- How long the cursor must stay still before the selection is sent.
  selection_debounce_ms = 100,
  -- Called each time the registration has been accepted.
  on_registered = nil,
})
```

## Main windows only

Each Neovim that calls `setup` registers a workspace of its own, which Claude Code lists in `/ide`. A Neovim that is
a child of another one (a window for a Git graph or for a diff, in a setup that opens those in a Neovim of their own)
should not appear there. The adapter cannot know which Neovim is which; `should_register` is how a configuration says
so. It is asked once, when the adapter starts:

```lua
require('claude_ide').setup({
  should_register = function()
    return not vim.g.my_window_is_a_child -- however your configuration marks those
  end,
})
```

## Commands

`:ClaudeIde status` shows whether the adapter is connected, the port of the workspace, the socket, and the last
problem. `:ClaudeIde start` and `:ClaudeIde stop` start and stop the connection.

## How it connects

Nobody has to start the server by hand. The adapter connects to the socket of the server, and what happens when
nobody is listening depends on how the server is set up:

- **A supervisor holds the socket** (systemd socket activation, see `contrib/`): the connection works, and the first
  one starts the server. The adapter never notices that the server was not running.
- **Nothing holds it:** the adapter starts `claude-ide-server` itself, once, and then waits for it like for any other
  server. The server is not a child of Neovim: it goes on when Neovim ends, and ends by itself when no adapter has
  been there for a while (`--idle-timeout`, five minutes as the adapter starts it). Several Neovims that start at the
  same time all try, and the server refuses to start twice on one socket, so one wins and the others connect to it.
  `server.autostart = false` turns this off; `server.command` and `server.args` say what to run (the executable is
  looked up on the `PATH`; the output of a detached process has nowhere to go, so a wrapper of your own is the way to
  keep its log). If the executable is not found, the user is told, once; if the server that was started ends with an
  error before it can be reached, the exit code is in the last problem that `:ClaudeIde status` shows, and a server that
  did not understand its command line (exit code 2 or more) is reported with a message.

Either way the adapter keeps trying: after `reconnect.initial_ms`, then after twice that, and so on up to
`reconnect.max_ms`, without a message, so a Neovim that starts before the server is not noisy. When the server says
`shutdown` (it is stopping) or closes the connection, the adapter does the same, which starts the server again under
socket activation (stop the socket unit as well to stop it for good). A server that refuses `register` (a protocol
version that it does not know, for one) is not the same as one that is not there: trying again changes nothing, so the
adapter tells the user, once.

After each registration the current selection is sent once, so that the server, and a Claude Code that connects to
it, start from what the user has now.

## The selection

`selectionChanged` is sent once the cursor has stayed still for `selection_debounce_ms` and the selection is not the
one that was sent last. A selection is the visual selection, or an empty range at the cursor. Positions are
`{ line, character }` with `line` 0-based and `character` in UTF-16 code units (Neovim counts bytes), and the end of a
range is exclusive. Buffers that are not files (a terminal, help, a buffer without a name) have none.

## Tests

```
nvim --headless -u NONE -l editors/nvim/tests/unit.lua       # what needs no server
cargo test -p claude-ide-server --test nvim                  # a headless Neovim against the real executable
```

The second needs `nvim` on the path. Without it the tests say so and pass, unless `CLAUDE_IDE_REQUIRE_NVIM` is set
(CI sets it).
