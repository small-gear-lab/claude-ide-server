--- What the requests have in common about files and buffers: which buffers are files, how a path finds its buffer, and
--- what language a file is in.
local M = {}

--- The ids that Claude Code gives to languages, where they differ from the names of Neovim's filetypes. **(assumption)**
--- Claude Code does not need them to be right (it shows them); only the ones that are known to differ are listed.
local language_ids = {
  sh = 'shellscript',
  bash = 'shellscript',
  zsh = 'shellscript',
  text = 'plaintext',
  cs = 'csharp',
  tex = 'latex',
  make = 'makefile',
  dosbatch = 'bat',
  objc = 'objective-c',
  objcpp = 'objective-cpp',
}

---@param filetype string
---@return string
function M.language_id(filetype)
  if filetype == '' then
    return 'plaintext'
  end
  return language_ids[filetype] or filetype
end

--- The language of a buffer: its `filetype`, or, for a buffer that is not loaded yet (one that was only added to the
--- list) and has none, the one that its name says.
---@param buf integer
---@param path string
---@return string
function M.language_of(buf, path)
  local filetype = vim.bo[buf].filetype
  if filetype == '' then
    filetype = vim.filetype.match({ filename = path }) or ''
  end
  return M.language_id(filetype)
end

--- Whether a buffer is a file that Claude can be told about: it has a name and is a normal buffer. (A buffer without
--- a name has no path to name it by; a terminal or the help are not files.)
---@param buf integer
---@return string? path The absolute path, or nil.
function M.file_of(buf)
  local name = vim.api.nvim_buf_get_name(buf)
  if name == '' or vim.bo[buf].buftype ~= '' then
    return nil
  end
  return M.long_path(vim.fn.fnamemodify(name, ':p'))
end

--- Whether a path starts at a root, as Claude Code (which is Node) takes it: with a separator, or on Windows with a drive and
--- a separator too (`C:\a`).
---@param path string
---@return boolean
function M.is_absolute(path)
  if path:sub(1, 1) == '/' then
    return true
  end
  return vim.fn.has('win32') == 1 and (path:match('^[\\/]') ~= nil or path:match('^%a:[\\/]') ~= nil)
end

--- On Windows, the path with the names written out in full and in their own case: Neovim keeps the name that it was given,
--- which may be the short form of 8.3 (`C:\Users\RUNNER~1`), and Claude Code compares the paths with its own working directory as
--- text, which has the long form. Anywhere else the path is as it is (Neovim names a file by its real path there already).
---@param path string
---@return string
function M.long_path(path)
  if vim.fn.has('win32') == 1 then
    return vim.uv.fs_realpath(path) or path
  end
  return path
end

--- The working directory, as a path to report (see `long_path`).
---@return string
function M.cwd()
  return M.long_path(vim.uv.cwd())
end

--- How many lines a document has, counted as the VS Code extension counts them: the empty line after a final newline is
--- one too (Neovim does not show it, and keeps the newline in `eol`).
---@param buf integer
---@return integer
function M.line_count(buf)
  return vim.api.nvim_buf_line_count(buf) + (vim.bo[buf].eol and 1 or 0)
end

--- The same file, however it was named (through a link, or not): the real path if there is one.
---@param path string
---@return string
local function real(path)
  return vim.uv.fs_realpath(path) or path
end

--- The buffer that holds the file at `path` (absolute), if there is one. With `loaded`, only a buffer whose text is in
--- memory counts, which is what "open" means for a document.
---@param path string
---@param loaded boolean?
---@return integer?
function M.buffer_for(path, loaded)
  local wanted = real(path)
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local name = M.file_of(buf)
    if name and (not loaded or vim.api.nvim_buf_is_loaded(buf)) and (name == path or real(name) == wanted) then
      return buf
    end
  end
  return nil
end

--- A window to show a file in: the current one if it shows a file; else the one that the user came from, or any that
--- shows a file (the focus may be in a terminal, where a file should not be opened over what runs there). Nil if there
--- is none, and the caller then uses the current one.
---@return integer?
function M.window_for_files()
  local function shows_a_file(win)
    return vim.api.nvim_win_is_valid(win)
      and vim.api.nvim_win_get_config(win).relative == ''
      and vim.bo[vim.api.nvim_win_get_buf(win)].buftype == ''
  end
  local current = vim.api.nvim_get_current_win()
  if shows_a_file(current) then
    return current
  end
  local previous = vim.fn.win_getid(vim.fn.winnr('#'))
  if previous ~= 0 and shows_a_file(previous) then
    return previous
  end
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if shows_a_file(win) then
      return win
    end
  end
  return nil
end

return M
