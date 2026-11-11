--- Telling the server when diagnostics change: `diagnosticsChanged` with the URIs of the files whose diagnostics changed.
---
--- Diagnostics change in bursts (a language server answers for a file, then for another), so the files are collected for
--- `debounce_ms` and sent together, once.
local files = require('claude_ide.files')

local M = {}

--- Sends `diagnosticsChanged` for the files whose diagnostics change. Returns a table with `detach()`.
---@param session claude_ide.Session
---@param group integer The autocommand group.
---@param debounce_ms integer
---@return { detach: fun() }
function M.attach(session, group, debounce_ms)
  local timer = assert(vim.uv.new_timer())
  ---@type string[]
  local pending = {}
  ---@type table<string, boolean>
  local seen = {}

  local send = vim.schedule_wrap(function()
    if #pending == 0 then
      return
    end
    local uris = pending
    pending, seen = {}, {}
    -- Not sent when there is no registered connection: it is an event, and a server that was not there for it is not
    -- told of it (a Claude Code asks for the diagnostics when it needs them).
    session:notify('diagnosticsChanged', { uris = uris })
  end)

  vim.api.nvim_create_autocmd('DiagnosticChanged', {
    group = group,
    callback = function(args)
      local path = args.buf and files.file_of(args.buf)
      if not path then
        return
      end
      local uri = vim.uri_from_fname(path)
      if not seen[uri] then
        seen[uri] = true
        table.insert(pending, uri)
      end
      timer:stop()
      timer:start(debounce_ms, 0, send)
    end,
  })

  return {
    detach = function()
      timer:stop()
      timer:close()
    end,
  }
end

return M
