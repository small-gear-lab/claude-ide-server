--- What is selected in the current buffer, in the form that the server takes (docs/DESIGN.md, "What an adapter
--- reports of the editor"), and sending it when it changes.
---
--- Positions are `{ line, character }`: `line` 0-based, `character` in UTF-16 code units (Neovim counts bytes), and the
--- end of a range is exclusive. A cursor without a selection is an empty range at the cursor.
local M = {}

--- The column in UTF-16 code units of the byte at `byte_col` (0-based) of `line`.
---@param line string
---@param byte_col integer
---@return integer
function M.utf16_col(line, byte_col)
  return vim.str_utfindex(line, 'utf-16', math.min(byte_col, #line), false)
end

---@param line string
---@param byte_col integer 0-based
local function position(lnum, line, byte_col)
  return { line = lnum - 1, character = M.utf16_col(line, byte_col) }
end

--- What the editor is doing: the mode, the other end of a visual selection (`v`), and the cursor (`.`), as `getpos`
--- gives them. Taken from the editor unless given (the tests give them).
---@class claude_ide.EditorState
---@field mode string
---@field visual integer[]
---@field cursor integer[]

---@return claude_ide.EditorState
local function editor_state()
  return { mode = vim.fn.mode(), visual = vim.fn.getpos('v'), cursor = vim.fn.getpos('.') }
end

---@alias claude_ide.Selection { filePath: string, text: string, selection: { start: table, ['end']: table } }

--- The selection of the buffer `bufnr`, or nil if it is not a file (a terminal, help, a buffer without a name): Claude
--- has nothing to look at there.
---@param bufnr integer?
---@param state claude_ide.EditorState?
---@return claude_ide.Selection?
function M.current(bufnr, state)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  state = state or editor_state()
  local files = require('claude_ide.files')
  local path = files.file_of(bufnr)
  if not path or not files.is_absolute(path) then
    return nil
  end

  local function line_of(lnum)
    return vim.api.nvim_buf_get_lines(bufnr, lnum - 1, lnum, false)[1] or ''
  end

  local mode = state.mode:sub(1, 1)
  if mode == 'v' or mode == 'V' or mode == '\22' then
    local regions = vim.fn.getregionpos(state.visual, state.cursor, { type = state.mode:sub(1, 1) })
    if #regions > 0 then
      local first, last = regions[1][1], regions[#regions][2]
      -- `getregionpos` gives the end as the last byte of the last character (1-based), which is the exclusive end
      -- of a 0-based range.
      local text = table.concat(vim.fn.getregion(state.visual, state.cursor, { type = state.mode:sub(1, 1) }), '\n')
      return {
        filePath = path,
        text = text,
        selection = {
          start = position(first[2], line_of(first[2]), first[3] - 1),
          ['end'] = position(last[2], line_of(last[2]), last[3]),
        },
      }
    end
  end

  local lnum, col = state.cursor[2], state.cursor[3] - 1
  local here = position(lnum, line_of(lnum), col)
  return { filePath = path, text = '', selection = { start = here, ['end'] = here } }
end

---@type claude_ide.Selection?
local latest

--- The last selection that there was in a file, kept while the focus is somewhere that has none (a terminal, a help
--- page): what Claude should hear about is the file that the user was in.
---@return claude_ide.Selection?
function M.latest()
  return latest
end

--- The selection to answer a request with: the one of the current buffer, or, if that is not a file, the last one.
---@return claude_ide.Selection?
function M.available()
  local selection = M.current()
  if selection then
    latest = selection
    return selection
  end
  return latest
end

--- Sends `selectionChanged` when the selection of the current buffer changes, once things have settled for
--- `debounce_ms` (the cursor moves all the time). Returns a table: `flush()` sends the current selection now, even if it
--- is the one that was sent last (for a server that has just been connected to), and `detach()` stops.
---@param session claude_ide.Session
---@param group integer The autocommand group.
---@param debounce_ms integer
---@return { flush: fun(), detach: fun() }
function M.attach(session, group, debounce_ms)
  local timer = assert(vim.uv.new_timer())
  ---@type claude_ide.Selection?
  local last_sent

  local function send(force)
    local selection = M.current()
    if selection then
      latest = selection
    end
    if not selection or (not force and vim.deep_equal(selection, last_sent)) then
      return
    end
    -- Not sent when there is no registered connection: the next registration sends it.
    if session:notify('selectionChanged', selection) then
      last_sent = selection
    end
  end

  local settle = vim.schedule_wrap(function()
    send(false)
  end)
  vim.api.nvim_create_autocmd({ 'CursorMoved', 'CursorMovedI', 'ModeChanged', 'BufEnter', 'WinEnter' }, {
    group = group,
    callback = function()
      timer:stop()
      timer:start(debounce_ms, 0, settle)
    end,
  })

  return {
    flush = function()
      send(true)
    end,
    detach = function()
      timer:stop()
      timer:close()
    end,
  }
end

return M
