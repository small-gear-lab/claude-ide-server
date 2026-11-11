--- Connects Neovim to Claude Code's `/ide` through `claude-ide-server`.
local Session = require('claude_ide.session')
local diagnostics = require('claude_ide.diagnostics')
local diff = require('claude_ide.diff')
local methods = require('claude_ide.methods')
local selection = require('claude_ide.selection')

local M = {}

---@class claude_ide.Options
---@field socket string? The server's adapter socket. Default: where the server puts it.
---@field ide_name string The name that `/ide` shows.
---@field should_register fun(): boolean Whether this Neovim is to register (see the README: main windows only).
---@field workspace_folders fun(): string[] The folders of the workspace. Default: the working directory.
---@field reconnect { initial_ms: integer, max_ms: integer } The wait between attempts to connect: doubles up to the maximum.
---@field server claude_ide.ServerOptions Whether and how to start the server when nobody is listening.
---@field register_timeout_ms integer How long the server has to answer `register`.
---@field selection_debounce_ms integer How long the cursor must stay still before the selection is sent.
---@field diagnostics_debounce_ms integer How long diagnostics are collected before the files that changed are reported.
---@field diagnostics_wait false|{ first_ms: integer?, quiet_ms: integer?, cap_ms: integer?, cap_hidden_ms: integer? } How long `getDiagnostics` waits for the diagnostics of a file that was changed under Neovim (see `methods.wait`). `false`: not at all.
---@field diff false|{ provider: ('tab'|claude_ide.DiffProvider)?, keys: { accept: string?, reject: string? }? } How the changes that Claude Code proposes are shown (`false`: not at all: Claude Code then shows them itself).
---@field methods table<string, fun(params: any): any, table?> What the server may ask of this Neovim.
---@field on_gone fun()? Called when the connection to the server has ended.
---@field on_registered fun(session: claude_ide.Session)? Called each time the registration has been accepted.
local defaults = {
  ide_name = 'Neovim',
  should_register = function()
    return true
  end,
  workspace_folders = function()
    return { require('claude_ide.files').cwd() }
  end,
  reconnect = { initial_ms = 500, max_ms = 10000 },
  server = {
    autostart = true,
    command = 'claude-ide-server',
    -- It ends by itself when no adapter has been there for five minutes.
    args = { '--idle-timeout', '300' },
  },
  register_timeout_ms = 5000,
  selection_debounce_ms = 100,
  diagnostics_debounce_ms = 100,
  diagnostics_wait = {},
  diff = { provider = 'tab', keys = {} },
  methods = methods.handlers,
}

---@type claude_ide.Session?
local session
---@type { flush: fun(), detach: fun() }?
local selecting
---@type { detach: fun() }?
local diagnosing

local group = vim.api.nvim_create_augroup('claude_ide', { clear = true })

--- The workspace follows the working directory of the whole Neovim: `:cd`, not `:lcd` or `:tcd`, which belong to a
--- window or a tab.
local function follow_folders()
  vim.api.nvim_create_autocmd('DirChanged', {
    group = group,
    pattern = 'global',
    callback = function()
      if session then
        session:notify('workspaceFoldersChanged', { workspaceFolders = session.opts.workspace_folders() })
      end
    end,
  })
end

--- Sets up and, unless `autostart = false`, starts connecting once Neovim has started.
---@param opts (claude_ide.Options | { autostart: boolean? })?
function M.setup(opts)
  opts = vim.tbl_deep_extend('force', defaults, opts or {})
  if session then
    session:stop()
  end
  if selecting then
    selecting.detach()
  end
  if diagnosing then
    diagnosing.detach()
  end
  vim.api.nvim_clear_autocmds({ group = group })
  if opts.diagnostics_wait == false then
    methods.wait = false
  else
    methods.wait = vim.tbl_extend('force', { first_ms = 750, quiet_ms = 150, cap_ms = 1500, cap_hidden_ms = 1000 }, opts.diagnostics_wait)
  end
  -- The methods are this Neovim's own copy, so that leaving some out does not change the module's.
  opts.methods = vim.tbl_extend('force', {}, opts.methods)
  if opts.diff == false then
    for _, name in ipairs({ 'openDiff', 'closeDiff', 'closeAllDiffs' }) do
      opts.methods[name] = nil
    end
  else
    diff.configure(opts.diff)
  end
  -- Diffs that wait for a server that is gone are over.
  local on_gone = opts.on_gone
  opts.on_gone = function()
    diff.cancel_all()
    if on_gone then
      on_gone()
    end
  end
  local on_registered = opts.on_registered
  opts.on_registered = function(registered)
    -- A server that has just been connected to knows no selection: tell it the one that there is.
    if selecting then
      selecting.flush()
    end
    if on_registered then
      on_registered(registered)
    end
  end
  session = Session.new(opts)
  selecting = selection.attach(session, group, opts.selection_debounce_ms)
  diagnosing = diagnostics.attach(session, group, opts.diagnostics_debounce_ms)
  follow_folders()
  if opts.autostart == false then
    return
  end
  if vim.v.vim_did_enter == 1 then
    M.start()
  else
    vim.api.nvim_create_autocmd('VimEnter', {
      once = true,
      group = group,
      callback = M.start,
    })
  end
end

--- Mentions the current file to Claude Code (an @-mention): the lines `first` to `last` (1-based, as in `:help
--- :range`), or the whole file. Returns whether Claude Code was told, and if not, why.
---@param first integer?
---@param last integer?
---@return boolean sent
---@return string? why
function M.mention(first, last)
  local buf = vim.api.nvim_get_current_buf()
  local path = methods.file_of(buf)
  if not path then
    return false, 'this buffer is not a file'
  end
  first = first or 1
  last = last or vim.api.nvim_buf_line_count(buf)
  if not session or not session:notify('atMention', { filePath = path, lineStart = first - 1, lineEnd = last - 1 }) then
    return false, 'Neovim is not connected to the server'
  end
  return true
end

--- Starts connecting. Returns false if this Neovim is not to register (`should_register`).
---@return boolean
function M.start()
  if not session then
    M.setup({ autostart = false })
  end
  if not session.opts.should_register() then
    return false
  end
  session:start()
  return true
end

function M.stop()
  if session then
    session:stop()
  end
end

---@return { state: string, port: integer?, socket: string?, attempts: integer?, last_error: string? }
function M.status()
  if not session then
    return { state = 'stopped' }
  end
  return {
    state = session.state,
    port = session.port,
    socket = session:socket(),
    attempts = session.attempts,
    last_error = session.last_error,
  }
end

return M
