--- Showing the changes that Claude Code proposes (`openDiff`) and waiting for the user to accept or reject them.
---
--- This module has the flow: a diff waits for the user (the request of the server is not answered until then, and the rest
--- of Neovim goes on), a diff has a name (`tabName`), a new diff of the same name replaces the one before it, and a diff
--- can be closed from Claude Code (`closeDiff`, `closeAllDiffs`). *Showing* a diff, and what the user does to decide, is
--- the job of a provider, which a configuration can give (see `diff_tab.lua` for the one that is used unless it does).
---
--- A provider is a table with
---   `open(diff, finish) -> handle`: shows `diff` (`{ tab_name, old_path, new_path, new_contents }`), and calls
---       `finish(true, contents)` when the user accepts (`contents` are the contents of the file as they are then, which the
---       user may have edited) or `finish(false)` when the user rejects. Called once at most. The handle that it returns is
---       a table, which it can fill as it likes; `accept()` and `reject()` in it are for the commands
---       `:ClaudeIdeDiffAccept` and `:ClaudeIdeDiffReject`.
---   `close(handle)`: takes the diff that `open` gave this handle off the screen. Called when the diff is over, however it
---       ended. (By the handle and not by name: a newer diff of the same name may be on the screen by then.)
local M = {}

---@class claude_ide.DiffSpec
---@field tab_name string
---@field old_path string
---@field new_path string
---@field new_contents string

---@class claude_ide.DiffProvider
---@field open fun(diff: claude_ide.DiffSpec, finish: fun(accepted: boolean, contents: string?)): table
---@field close fun(handle: table)

local defaults = { provider = 'tab', keys = {} }
local config = vim.deepcopy(defaults)

---@class claude_ide.PendingDiff
---@field id integer? The number that the server gave this diff.
---@field name string
---@field co thread The request that waits.
---@field done boolean
---@field closed boolean? Whether its screen has been taken away.
---@field close_ui fun()
---@field handle table? What the provider gave.
---@field finish fun(accepted: boolean, contents: string?)

---@type table<string, claude_ide.PendingDiff>
local pending = {}

---@param options { provider: ('tab'|claude_ide.DiffProvider)?, keys: { accept: string?, reject: string? }? }?
function M.configure(options)
  config = vim.tbl_deep_extend('force', defaults, options or {})
end

---@return claude_ide.DiffProvider
function M.provider()
  local provider = config.provider
  if provider == nil or provider == 'tab' then
    return require('claude_ide.diff_tab')
  end
  return provider
end

--- The keys that the user chose for accepting and rejecting (a provider may map them).
---@return { accept: string?, reject: string? }
function M.keys()
  return config.keys or {}
end

local function failed(message)
  return nil, vim.lsp.rpc.rpc_response_error(vim.lsp.protocol.ErrorCodes.InvalidParams, message)
end

--- `openDiff`: shows the diff, and answers when the user has decided: `{ accepted = true, contents }` or
--- `{ accepted = false }`. The server does not time this out; what is waiting here is the request, not Neovim.
---@param params { tabName: string, oldFilePath: string, newFilePath: string, newFileContents: string, diffId: integer? }
function M.openDiff(params)
  local name = params.tabName
  if type(name) ~= 'string' or name == '' then
    return failed('tabName is required')
  end
  if not coroutine.isyieldable() then
    return failed('openDiff cannot wait for the user here')
  end
  -- A diff of the same name is replaced: the one before it is over, rejected.
  local older = pending[name]
  if older then
    older.finish(false)
    -- Its screen goes now, not later: the new diff is about to be shown under the same name.
    older.close_ui()
  end

  local entry = { id = params.diffId, name = name, co = coroutine.running(), done = false }
  pending[name] = entry
  --- Takes the diff off the screen, once.
  entry.close_ui = function()
    if entry.handle and not entry.closed then
      entry.closed = true
      pcall(M.provider().close, entry.handle)
    end
  end
  entry.finish = function(accepted, contents)
    if entry.done then
      return
    end
    entry.done = true
    if pending[name] == entry then
      pending[name] = nil
    end
    local outcome = accepted and { accepted = true, contents = contents or '' } or { accepted = false }
    -- Not in the middle of what the user did (a write, a close): after it.
    vim.schedule(function()
      entry.close_ui()
      coroutine.resume(entry.co, outcome)
    end)
  end

  local spec = {
    tab_name = name,
    old_path = params.oldFilePath,
    new_path = params.newFilePath,
    new_contents = params.newFileContents or '',
  }
  local ok, handle = pcall(M.provider().open, spec, entry.finish)
  if not ok then
    pending[name] = nil
    return failed('could not show the diff: ' .. tostring(handle))
  end
  -- A provider that gives nothing back still gets its `close`, with an empty handle.
  entry.handle = type(handle) == 'table' and handle or {}
  return coroutine.yield()
end

--- `closeDiff`: takes a diff off the screen, which rejects it. With `diffId`, only the diff of that number (a call of
--- Claude Code that is gone closes its own diff, not a newer one of the same name); without, whichever has the name.
---@param params { tabName: string, diffId: integer? }
function M.closeDiff(params)
  local entry = pending[params.tabName]
  if entry and (params.diffId == nil or params.diffId == entry.id) then
    entry.finish(false)
    return { closed = true }
  end
  return { closed = false }
end

--- `closeAllDiffs`: closes every diff, and says how many there were.
function M.closeAllDiffs()
  local entries = vim.tbl_values(pending)
  for _, entry in ipairs(entries) do
    entry.finish(false)
  end
  return { closed = #entries }
end

--- Ends every diff, rejected: for when the connection to the server is gone, and nobody is waiting for them any more.
function M.cancel_all()
  for _, entry in ipairs(vim.tbl_values(pending)) do
    entry.finish(false)
  end
end

--- The diff that a command is about: the one named, or else the one of the current buffer, or else the only one.
---@param name string?
---@return claude_ide.PendingDiff?
---@return string? why
local function find(name)
  name = name or vim.b.claude_ide_diff
  if name then
    return pending[name], 'there is no diff named ' .. name
  end
  local entries = vim.tbl_values(pending)
  if #entries == 1 then
    return entries[1]
  end
  return nil, #entries == 0 and 'there is no diff to decide' or 'there are several diffs: say which'
end

---@param name string?
---@return boolean ok
---@return string? why
function M.accept(name)
  local entry, why = find(name)
  if not entry then
    return false, why
  end
  if not (entry.handle and entry.handle.accept) then
    return false, 'this way of showing diffs has no accept command: use its own'
  end
  entry.handle.accept()
  return true
end

---@param name string?
---@return boolean ok
---@return string? why
function M.reject(name)
  local entry, why = find(name)
  if not entry then
    return false, why
  end
  entry.finish(false)
  return true
end

--- How many diffs wait for the user. For tests, and `:ClaudeIde status`.
---@return integer
function M.count()
  return #vim.tbl_keys(pending)
end

return M
