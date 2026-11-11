--- What the server may ask of this Neovim: the answers to its requests, in the shapes that the server accepts
--- (docs/DESIGN.md, "What an adapter reports of the editor"). Each takes the parameters of the request and returns the
--- result. The adapter tells the server which of these it has (`register`'s `methods`), so that the server offers Claude
--- only the tools that can be answered.
local selection = require('claude_ide.selection')

local M = {}

--- The requests that the server may make, by name: this is what `register` declares, so only these belong in it.
---@type table<string, fun(params: any): any>
local handlers = {}
M.handlers = handlers

local files = require('claude_ide.files')
local file_of = files.file_of

M.language_id = files.language_id
M.file_of = file_of


--- The open files: every listed buffer that is a file. The active one is the buffer of the current window, or, if that
--- is not a file, the file that the last selection was in.
---@return { tabs: table[] }
function handlers.getOpenEditors()
  local active = file_of(vim.api.nvim_get_current_buf())
  if not active then
    local latest = selection.latest()
    active = latest and latest.filePath
  end
  -- Where the files are shown: the window of the current tab page that shows each, in order (a window is a group of
  -- tabs, as in the editors that this protocol comes from).
  local shown = {}
  local current_win = vim.api.nvim_get_current_win()
  for index, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    local buf = vim.api.nvim_win_get_buf(win)
    if not shown[buf] and vim.api.nvim_win_get_config(win).relative == '' then
      shown[buf] = { group = index - 1, is_current = win == current_win }
    end
  end
  local latest = selection.available()

  local tabs = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local path = vim.bo[buf].buflisted and file_of(buf)
    if path then
      local tab = {
        uri = vim.uri_from_fname(path),
        isActive = path == active,
        label = vim.fn.fnamemodify(path, ':t'),
        languageId = files.language_of(buf, path),
        isDirty = vim.bo[buf].modified,
        -- What Neovim has no notion of: pinned tabs, and tabs that are previews.
        isPinned = false,
        isPreview = false,
        fileName = path,
        lineCount = files.line_count(buf),
        isUntitled = false,
      }
      local place = shown[buf]
      if place then
        tab.groupIndex = place.group
        tab.viewColumn = place.group + 1
        tab.isGroupActive = place.is_current
      end
      -- The selection is given for the active tab, as the extension does.
      if tab.isActive and latest and latest.filePath == path then
        tab.selection = {
          start = latest.selection.start,
          ['end'] = latest.selection['end'],
          isReversed = false,
        }
      end
      table.insert(tabs, tab)
    end
  end
  return { tabs = tabs }
end

--- A selection, or why there is none.
---@param found claude_ide.Selection?
---@return table
local function answer(found)
  if found then
    return found
  end
  return { success = false, message = 'No file is open' }
end

--- The selection in the file that the user is in (or was last in).
function handlers.getCurrentSelection()
  return answer(selection.available())
end

--- The last selection, even if the focus has left the files.
function handlers.getLatestSelection()
  return answer(selection.available())
end

local severities = {
  [vim.diagnostic.severity.ERROR] = 'Error',
  [vim.diagnostic.severity.WARN] = 'Warning',
  [vim.diagnostic.severity.INFO] = 'Information',
  [vim.diagnostic.severity.HINT] = 'Hint',
}

--- A diagnostic of Neovim in the form that the server takes. Neovim's positions are 0-based and count bytes, and the end
--- of its range is exclusive; the server's are 0-based and count UTF-16 code units, the end being exclusive too.
---@param diagnostic vim.Diagnostic
---@param line_of fun(lnum: integer): string The text of a line (0-based).
---@return table
function M.to_server_diagnostic(diagnostic, line_of)
  local end_lnum = diagnostic.end_lnum or diagnostic.lnum
  local end_col = diagnostic.end_col or diagnostic.col
  local result = {
    message = diagnostic.message,
    -- Neovim has no fifth level: anything that is not known counts as the least.
    severity = severities[diagnostic.severity] or 'Hint',
    range = {
      start = { line = diagnostic.lnum, character = selection.utf16_col(line_of(diagnostic.lnum), diagnostic.col) },
      ['end'] = { line = end_lnum, character = selection.utf16_col(line_of(end_lnum), end_col) },
    },
  }
  if diagnostic.source then
    result.source = diagnostic.source
  end
  return result
end

--- The diagnostics of a buffer, as the server takes them: `{ uri, diagnostics }`.
---@param buf integer
---@param path string
---@param uri string? The `uri` that the file is to go by: the one that was asked for, else the one of the real path.
---@return table
local function diagnostics_of(buf, path, uri)
  local function line_of(lnum)
    return vim.api.nvim_buf_get_lines(buf, lnum, lnum + 1, false)[1] or ''
  end
  local found = {}
  for _, diagnostic in ipairs(vim.diagnostic.get(buf)) do
    table.insert(found, M.to_server_diagnostic(diagnostic, line_of))
  end
  return { uri = uri or vim.uri_from_fname(path), diagnostics = found }
end

--- How long `getDiagnostics` waits for the diagnostics of a file that has just been changed under Neovim. The times are those of
--- the VS Code extension, which waits for the editor to publish the diagnostics of a file that Claude Code has edited: up to
--- `first_ms` for the first update (none by then: the file has none to report), then until things are quiet for `quiet_ms`,
--- and in all no longer than `cap_ms` (`cap_hidden_ms` for a file that no window shows). `false` does not wait.
---@type false|{ first_ms: integer, quiet_ms: integer, cap_ms: integer, cap_hidden_ms: integer }
M.wait = { first_ms = 750, quiet_ms = 150, cap_ms = 1500, cap_hidden_ms = 1000 }

--- Makes the buffer of a file that was written by someone else (Claude Code, say) hold what is on the disk now: a buffer without
--- changes that is older than the file is read again, and then whatever makes diagnostics (a language server, a linter) is told of the
--- change by Neovim as for any other. A buffer with changes that were not written is the user's, and is left alone.
---@param buf integer
---@param path string
---@return boolean reloaded
local function reload_if_older(buf, path)
  if vim.bo[buf].modified or vim.uv.fs_stat(path) == nil then
    return false
  end
  if vim.deep_equal(vim.fn.readfile(path), vim.api.nvim_buf_get_lines(buf, 0, -1, false)) then
    return false
  end
  vim.api.nvim_buf_call(buf, function()
    vim.cmd('silent! edit')
  end)
  return true
end

--- Lets the editor publish the diagnostics of a buffer that was read again, as the VS Code extension does for a file that was
--- changed: it returns (in the request that waits: a coroutine) when they have come and things are quiet, or when time is up.
---@param buf integer
local function wait_for_diagnostics(buf)
  local wait = M.wait
  if not wait or not coroutine.isyieldable() then
    return
  end
  local shown = false
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    shown = shown or vim.api.nvim_win_get_buf(win) == buf
  end
  local cap = shown and wait.cap_ms or wait.cap_hidden_ms
  local co = coroutine.running()
  local started = vim.uv.now()
  local timer = assert(vim.uv.new_timer())
  local finished = false
  local autocmd
  local function finish()
    if finished then
      return
    end
    finished = true
    timer:stop()
    timer:close()
    pcall(vim.api.nvim_del_autocmd, autocmd)
    vim.schedule(function()
      coroutine.resume(co)
    end)
  end
  local function rearm(after)
    timer:stop()
    timer:start(math.max(after, 1), 0, finish)
  end
  rearm(math.min(wait.first_ms, cap))
  autocmd = vim.api.nvim_create_autocmd('DiagnosticChanged', {
    buffer = buf,
    callback = function()
      -- An update: wait until things are quiet, and not longer than the cap allows.
      rearm(math.min(wait.quiet_ms, cap - (vim.uv.now() - started)))
    end,
  })
  coroutine.yield()
end

--- The diagnostics of one file (`params.uri`), or of every open file that has some. For one file that was changed on the disk
--- since Neovim read it, the file is read again and the diagnostics are waited for first: Claude Code asks for them right after
--- it has edited the file, and what Neovim has by then is those of the text before the edit.
---@param params { uri: string? }?
---@return table[]
function handlers.getDiagnostics(params)
  local wanted = type(params) == 'table' and type(params.uri) == 'string' and params.uri or nil
  if wanted then
    -- One file: found by its real path (the path that was asked for may go through a link), and answered under the `uri`
    -- that it was asked for, as it is spelt there. Claude Code compares that `uri` with the path of the file that it edited
    -- (it does not resolve links) and drops what does not match it, so what it gave is what it must get back.
    local path = vim.uri_to_fname(wanted)
    -- A file that is in the list is answered, with no diagnostics if it has none, whether or not its text is loaded yet.
    local buf = files.buffer_for(path)
    if not buf then
      return {}
    end
    if vim.api.nvim_buf_is_loaded(buf) and reload_if_older(buf, file_of(buf) or path) then
      wait_for_diagnostics(buf)
    end
    return { diagnostics_of(buf, file_of(buf) or path, wanted) }
  end
  local result = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local path = file_of(buf)
    if path and #vim.diagnostic.get(buf) > 0 then
      table.insert(result, diagnostics_of(buf, path))
    end
  end
  return result
end

-- What acts on a file: `openFile`, `checkDocumentDirty`, `saveDocument`.
local actions = require('claude_ide.actions')
for _, name in ipairs({ 'openFile', 'checkDocumentDirty', 'saveDocument' }) do
  handlers[name] = actions[name]
end

-- What shows the changes that Claude Code proposes: `openDiff`, `closeDiff`, `closeAllDiffs`.
local diff = require('claude_ide.diff')
handlers.openDiff = diff.openDiff
handlers.closeDiff = diff.closeDiff
handlers.closeAllDiffs = diff.closeAllDiffs

return M
