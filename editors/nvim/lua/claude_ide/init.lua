--- Connects Neovim to Claude Code's `/ide` through `claude-ide-server`.
local Session = require('claude_ide.session')

local M = {}

---@class claude_ide.Options
---@field socket string? The server's adapter socket. Default: where the server puts it.
---@field ide_name string The name that `/ide` shows.
---@field should_register fun(): boolean Whether this Neovim is to register (see the README: main windows only).
---@field workspace_folders fun(): string[] The folders of the workspace. Default: the working directory.
---@field reconnect { initial_ms: integer, max_ms: integer } The wait between attempts to connect: doubles up to the maximum.
---@field register_timeout_ms integer How long the server has to answer `register`.
---@field methods table<string, fun(params: any): any, table?> What the server may ask of this Neovim.
---@field on_registered fun(session: claude_ide.Session)? Called each time the registration has been accepted.
local defaults = {
  ide_name = 'Neovim',
  should_register = function()
    return true
  end,
  workspace_folders = function()
    return { vim.uv.cwd() }
  end,
  reconnect = { initial_ms = 500, max_ms = 10000 },
  register_timeout_ms = 5000,
  methods = {},
}

---@type claude_ide.Session?
local session

--- Sets up and, unless `autostart = false`, starts connecting once Neovim has started.
---@param opts (claude_ide.Options | { autostart: boolean? })?
function M.setup(opts)
  opts = vim.tbl_deep_extend('force', defaults, opts or {})
  if session then
    session:stop()
  end
  session = Session.new(opts)
  if opts.autostart == false then
    return
  end
  if vim.v.vim_did_enter == 1 then
    M.start()
  else
    vim.api.nvim_create_autocmd('VimEnter', {
      once = true,
      group = vim.api.nvim_create_augroup('claude_ide_start', { clear = true }),
      callback = M.start,
    })
  end
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
