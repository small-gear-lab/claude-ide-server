--- The requests that act on a file: `openFile`, `checkDocumentDirty` and `saveDocument`. They are given an absolute path
--- (the server makes a relative one absolute from the first folder of the workspace) and answer in the shapes of
--- docs/DESIGN.md, "What an adapter reports of the editor".
local files = require('claude_ide.files')

local M = {}

---@param message string
local function failed(message)
  return nil, vim.lsp.rpc.rpc_response_error(vim.lsp.protocol.ErrorCodes.InvalidParams, message)
end

--- Where a byte offset of the text of a buffer (its lines joined with newlines) is: the line (0-based) and the column
--- (0-based, in bytes).
---@param lines string[]
---@param offset integer
---@return integer line
---@return integer column
function M.position_at(lines, offset)
  local rest = offset
  for index, line in ipairs(lines) do
    if rest <= #line then
      return index - 1, rest
    end
    rest = rest - #line - 1
  end
  local last = #lines
  return math.max(last - 1, 0), #(lines[last] or '')
end

--- The first place where `startText` is, and, if given, `endText` after it. What came of it, as the word that the server
--- takes (`text`, `range`, `positioned`, `not_found`), and the two ends: the start, and the last character of what is
--- to be selected (byte columns, 0-based), both as `{ line, column }`.
---@param lines string[]
---@param start_text string
---@param end_text string?
---@param to_end_of_line boolean
---@return string match
---@return integer[]? start
---@return integer[]? last
function M.find_text(lines, start_text, end_text, to_end_of_line)
  local text = table.concat(lines, '\n')
  local from = text:find(start_text, 1, true)
  if not from then
    return 'not_found'
  end
  local start_line, start_col = M.position_at(lines, from - 1)
  local start = { start_line, start_col }

  -- The byte offset after the last byte of what is selected, or nil if it is only the place of the start.
  local stop
  local match = 'text'
  if end_text then
    local rest = from - 1 + #start_text
    local found = text:find(end_text, rest + 1, true)
    if not found then
      return 'positioned', start
    end
    stop = found - 1 + #end_text
    match = 'range'
  else
    stop = from - 1 + #start_text
  end

  -- The last character, not the byte after it: a selection of Neovim includes the character that the cursor is on.
  local last_line, last_col = M.position_at(lines, math.max(stop - 1, 0))
  local line = lines[last_line + 1] or ''
  if #line > 0 then
    last_col = math.min(last_col, #line - 1)
    last_col = last_col + vim.str_utf_start(line, last_col + 1)
    if to_end_of_line and match == 'range' then
      last_col = #line - 1
      last_col = last_col + vim.str_utf_start(line, last_col + 1)
    end
  end
  return match, start, { last_line, last_col }
end

--- Shows what was found, in the window `win`: the cursor at the start, and the text selected (visual mode) if the window
--- has the focus (a selection can only be made in the window that the user is in).
---@param win integer
---@param start integer[]
---@param last integer[]?
local function show(win, start, last)
  vim.api.nvim_win_set_cursor(win, { start[1] + 1, start[2] })
  if not last or vim.api.nvim_get_current_win() ~= win then
    return
  end
  -- Visual mode cannot be left on by a command that is run from here, so the marks are set and `gv` is typed.
  vim.cmd('normal! v\27')
  local buf = vim.api.nvim_win_get_buf(win)
  vim.fn.setpos("'<", { buf, start[1] + 1, start[2] + 1, 0 })
  vim.fn.setpos("'>", { buf, last[1] + 1, last[2] + 1, 0 })
  vim.api.nvim_feedkeys('gv', 'n', false)
end

--- `openFile`: opens the file, in a window that shows files, and selects the text that was asked for.
---@param params { filePath: string, startText: string?, endText: string?, selectToEndOfLine: boolean?, makeFrontmost: boolean? }
function M.openFile(params)
  local path = params.filePath
  local stat = type(path) == 'string' and vim.uv.fs_stat(path) or nil
  -- A directory is not a file that can be opened as one.
  if not stat or stat.type ~= 'file' then
    return failed('File not found: ' .. tostring(path))
  end
  local front = params.makeFrontmost ~= false
  local start_text = type(params.startText) == 'string' and params.startText ~= '' and params.startText or nil
  local end_text = type(params.endText) == 'string' and params.endText ~= '' and params.endText or nil

  local buf = files.buffer_for(path)
  if not buf then
    buf = vim.fn.bufadd(path)
  end
  vim.bo[buf].buflisted = true
  vim.fn.bufload(buf)

  -- The window that shows it: one that shows it already (of this tab page). If it is to be the frontmost one and none
  -- shows it, a window that shows files is given it, and the focus. If it is not, nothing is taken over to show it: the
  -- buffer is in the list, and that is all.
  local win
  for _, candidate in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_buf(candidate) == buf then
      win = candidate
      break
    end
  end
  if front then
    if not win then
      win = files.window_for_files()
      if not win then
        -- Nothing here shows files (the only window is a terminal, say): the file gets a window of its own, and what runs
        -- in the others is left alone.
        vim.cmd('split')
        win = vim.api.nvim_get_current_win()
      end
      vim.api.nvim_win_set_buf(win, buf)
    end
    vim.api.nvim_set_current_win(win)
  end

  local result = {
    filePath = files.file_of(buf) or path,
    languageId = files.language_of(buf, path),
    lineCount = files.line_count(buf),
    isDirty = vim.bo[buf].modified,
    isUntitled = false,
  }
  if start_text then
    local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
    local match, start, last = M.find_text(lines, start_text, end_text, params.selectToEndOfLine == true)
    if start and win then
      show(win, start, last)
      vim.api.nvim_win_call(win, function()
        vim.cmd('normal! zz')
      end)
    end
    result.selection = match
  end
  return result
end

--- `checkDocumentDirty`: whether the text of a file that is open has been changed and not written. A file that is not
--- open (its text is not in memory) is answered with `success = false`.
---@param params { filePath: string }
function M.checkDocumentDirty(params)
  local buf = type(params.filePath) == 'string' and files.buffer_for(params.filePath, true) or nil
  if not buf then
    return { success = false }
  end
  return { isDirty = vim.bo[buf].modified, isUntitled = false }
end

--- `saveDocument`: writes a file that is open and has been changed. `saved` says whether it was written: false if it had
--- no changes, or if writing failed. A file that is not open is answered with `success = false`.
---@param params { filePath: string }
function M.saveDocument(params)
  local buf = type(params.filePath) == 'string' and files.buffer_for(params.filePath, true) or nil
  if not buf then
    return { success = false }
  end
  if not vim.bo[buf].modified then
    return { saved = false }
  end
  vim.api.nvim_buf_call(buf, function()
    pcall(vim.cmd, 'silent write')
  end)
  return { saved = not vim.bo[buf].modified }
end

return M
