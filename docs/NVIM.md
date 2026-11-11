# The Neovim adapter

The Neovim side of the connection: it registers a Neovim with `claude-ide-server`, so that Claude Code's `/ide`
finds it. The adapter is the directory `editors/nvim/` of this repository, written in Lua.

**What works now:** everything of the protocol except diffs: connecting to the server (and again when it is not there or
goes away), registering the workspace, following `:cd`, sending the selection and the diagnostics that change,
mentioning a file or some lines to Claude Code, and answering its requests for the open editors, the selection, the
diagnostics, and for opening, checking and saving a file. It was tried with a real Claude Code (2.1.322). **What does
not yet:** diffs (issues #13 and #14).

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
  -- How long diagnostics are collected before the files that changed are reported (once, for all of them).
  diagnostics_debounce_ms = 100,
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

## What Claude Code can ask

The adapter answers these requests of the server (the `methods` that it declares in `register`; a configuration can
replace them or add to them with the `methods` option):

- `getOpenEditors`: the listed buffers that are files, with the active one (the file of the current window, or, when the
  focus is in a terminal or a help page, the file that the user was last in). A buffer without a name, a terminal and
  the help are not editors for Claude. The language of a file is its `filetype`, or the one that its name says for a
  buffer that is not loaded yet; a few filetypes are renamed to the language ids that Claude Code uses (`sh` is
  `shellscript`, no filetype is `plaintext`).
- `getCurrentSelection` and `getLatestSelection`: the selection of the file that the user is in, or the last one that
  there was while the focus is somewhere that has none.
- `openFile`: opens the file in a window that shows files (one that shows it already, or else one that shows another
  file; with the focus in a terminal and no such window, a new one, so that what runs in the terminal is left alone), gives
  it the focus unless `makeFrontmost` is false, and selects the text between `startText` and `endText`. The selection
  is made in the window that has the focus (Neovim has no selection in a window that does not), so with
  `makeFrontmost` false the cursor is put at the text and nothing is selected. A directory, or a file that is not there,
  is an error.
- `checkDocumentDirty` and `saveDocument`: for a file whose text is in memory. A file that is not open is answered with
  `success = false`. `saveDocument` writes only a file that has changes. `lineCount` counts the empty line after a final
  newline, as the VS Code extension does.
- `getDiagnostics`: the diagnostics of `vim.diagnostic`, for the file of a `uri` or for every open file that has some,
  with `Error`, `Warning`, `Information` or `Hint` as the severity and the columns in UTF-16 code units.

Claude Code 2.1.322 asks for `getDiagnostics` itself, around its own edits, and does not use the others. See "What
Claude Code sends on its own" in [DESIGN](DESIGN.md).

## Mentioning to Claude Code

`:ClaudeIdeMention` mentions the current file to Claude Code, and `:'<,'>ClaudeIdeMention` (or any range, as in
`:2,4ClaudeIdeMention`) the lines of the range. Claude Code puts it into its prompt as `@file#L2-4`. There is no key
mapped to it; map it as you like. `require('claude_ide').mention(first, last)` does the same from Lua, with the lines
(1-based) or without them for the whole file, and returns whether Claude Code was told.

## Tests

```
nvim --headless -u NONE -l editors/nvim/tests/unit.lua       # what needs no server
cargo test -p claude-ide-server --test nvim                  # a headless Neovim against the real executable
```

The second needs `nvim` on the path. Without it the tests say so and pass, unless `CLAUDE_IDE_REQUIRE_NVIM` is set
(CI sets it).
