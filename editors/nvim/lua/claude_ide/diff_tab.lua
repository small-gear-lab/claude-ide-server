--- The provider that shows a diff in a tab page of its own (see `diff.lua` for what a provider is). On the left, the file as
--- it is now (the open buffer, with changes that were not written, if there is one); on the right, what Claude Code proposes,
--- which can be edited.
---
--- Writing the right side (`:write`) accepts the diff, with the contents as they are then. Rejecting is closing the tab page
--- (or the right window), `:ClaudeIdeDiffReject`, or the key that the configuration gave. Nothing is written to the file: the
--- file is written by Claude Code, with the contents that it is given.
local files = require('claude_ide.files')

local M = {}

--- The lines of a text, and whether it ends with a newline (which Neovim keeps in 'eol', not as a line).
---@param text string
---@return string[] lines
---@return boolean eol
local function lines_of(text)
  local lines = vim.split(text, '\n', { plain = true })
  if #lines > 1 and lines[#lines] == '' then
    table.remove(lines)
    return lines, true
  end
  return lines, false
end

--- The lines of the file as it is now: the buffer that holds it if it has changes that were not written (those are what
--- the user has, and what Claude Code's change is to be shown against), and otherwise the file, which is the truth: a buffer
--- that has no changes may be older than the file (Claude Code, or another program, wrote it since), as the VS Code
--- extension, which uses the document only if it is dirty, has it. Nothing for a file that does not exist yet.
---@param path string
---@return string[]
local function current_lines(path)
  local buf = files.buffer_for(path, true)
  if buf and vim.bo[buf].modified then
    return vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  end
  if vim.uv.fs_stat(path) then
    return vim.fn.readfile(path)
  end
  return {}
end

--- A name for a buffer that no other buffer has: `title`, or with a number if that is taken (two diffs of one name may be
--- on the screen for a moment, and Neovim does not allow two buffers of one name).
---@param title string
---@return string
local function unique(title)
  local taken = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    taken[vim.api.nvim_buf_get_name(buf)] = true
  end
  local candidate, number = title, 1
  while taken[candidate] or taken[vim.fn.fnamemodify(candidate, ':p')] do
    number = number + 1
    candidate = ('%s #%d'):format(title, number)
  end
  return candidate
end

---@param buf integer
---@param title string What the buffer is called.
---@param tab_name string The name of the diff, which the buffer remembers (the commands use it).
---@param filetype string
local function scratch(buf, title, tab_name, filetype)
  vim.bo[buf].swapfile = false
  vim.bo[buf].bufhidden = 'wipe'
  vim.api.nvim_buf_set_name(buf, unique(title))
  vim.b[buf].claude_ide_diff = tab_name
  if filetype ~= '' then
    vim.bo[buf].filetype = filetype
  end
end

---@param diff claude_ide.DiffSpec
---@param finish fun(accepted: boolean, contents: string?)
---@return table
function M.open(diff, finish)
  local name = diff.tab_name
  local filetype = vim.filetype.match({ filename = diff.new_path }) or ''

  vim.cmd('tabnew')
  local tab = vim.api.nvim_get_current_tabpage()
  local left_win = vim.api.nvim_get_current_win()
  local left = vim.api.nvim_get_current_buf()
  scratch(left, '[claude] ' .. name .. ' (now)', name, filetype)
  vim.bo[left].buftype = 'nofile'
  vim.api.nvim_buf_set_lines(left, 0, -1, false, current_lines(diff.old_path))
  vim.bo[left].modifiable = false

  vim.cmd('rightbelow vertical new')
  local right_win = vim.api.nvim_get_current_win()
  local right = vim.api.nvim_get_current_buf()
  scratch(right, '[claude] ' .. name, name, filetype)
  vim.bo[right].buftype = 'acwrite'
  local lines, eol = lines_of(diff.new_contents)
  vim.api.nvim_buf_set_lines(right, 0, -1, false, lines)
  vim.bo[right].eol = eol
  vim.bo[right].modified = false
  vim.wo[right_win].winbar = 'Claude Code proposes this: :write accepts it; :ClaudeIdeDiffReject or closing this tab rejects it'

  for _, win in ipairs({ left_win, right_win }) do
    vim.api.nvim_win_call(win, function()
      vim.cmd('diffthis')
    end)
  end

  local done = false
  --- The user has decided: the contents are taken now, before anything is closed.
  ---@param accepted boolean
  local function decide(accepted)
    if done then
      return
    end
    done = true
    local contents
    if accepted and vim.api.nvim_buf_is_valid(right) then
      contents = table.concat(vim.api.nvim_buf_get_lines(right, 0, -1, false), '\n')
      if vim.bo[right].eol then
        contents = contents .. '\n'
      end
    end
    finish(accepted, contents)
  end

  -- Accepted: the right side is written (`:write`, `:wq`, `:x`); the write is taken over, nothing goes to a file.
  vim.api.nvim_create_autocmd('BufWriteCmd', {
    buffer = right,
    callback = function()
      vim.bo[right].modified = false
      decide(true)
    end,
  })
  -- Rejected: either side goes away without a decision (the tab, or a window, was closed).
  for _, buf in ipairs({ left, right }) do
    vim.api.nvim_create_autocmd('BufWipeout', {
      buffer = buf,
      callback = function()
        decide(false)
      end,
    })
  end

  -- The keys that the configuration chose, on both sides.
  local keys = require('claude_ide.diff').keys()
  for _, buf in ipairs({ left, right }) do
    if keys.accept then
      vim.keymap.set('n', keys.accept, function()
        decide(true)
      end, { buffer = buf, desc = 'Accept the change that Claude Code proposes' })
    end
    if keys.reject then
      vim.keymap.set('n', keys.reject, function()
        decide(false)
      end, { buffer = buf, desc = 'Reject the change that Claude Code proposes' })
    end
  end

  -- The cursor goes to the first change, on the side that can be edited.
  vim.api.nvim_set_current_win(right_win)
  pcall(vim.cmd, 'normal! gg]c')

  return {
    tab = tab,
    buffers = { left, right },
    accept = function()
      decide(true)
    end,
    reject = function()
      decide(false)
    end,
  }
end

--- Takes the diff that `handle` stands for off the screen.
---@param handle { tab: integer, buffers: integer[] }
function M.close(handle)
  -- The tab page goes, unless it is the only one (then only what it shows is wiped).
  if vim.api.nvim_tabpage_is_valid(handle.tab) and #vim.api.nvim_list_tabpages() > 1 then
    pcall(vim.cmd, 'tabclose ' .. vim.api.nvim_tabpage_get_number(handle.tab))
  end
  for _, buf in ipairs(handle.buffers) do
    if vim.api.nvim_buf_is_valid(buf) then
      pcall(vim.api.nvim_buf_delete, buf, { force = true })
    end
  end
end

return M
