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

--- The diagnostics of one file (`params.uri`), or of every open file that has some.
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
