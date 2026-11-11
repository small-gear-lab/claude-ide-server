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

--- Whether a buffer is a file that Claude can be told about: it has a name and is a normal buffer. (A buffer without
--- a name has no path to name it by; a terminal or the help are not files.)
---@param buf integer
---@return string? path The absolute path, or nil.
local function file_of(buf)
  local name = vim.api.nvim_buf_get_name(buf)
  if name == '' or vim.bo[buf].buftype ~= '' then
    return nil
  end
  return vim.fn.fnamemodify(name, ':p')
end

--- The open files: every listed buffer that is a file. The active one is the buffer of the current window, or, if that
--- is not a file, the file that the last selection was in.
---@return { tabs: table[] }
function handlers.getOpenEditors()
  local active = file_of(vim.api.nvim_get_current_buf())
  if not active then
    local latest = selection.latest()
    active = latest and latest.filePath
  end
  local tabs = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    local path = vim.bo[buf].buflisted and file_of(buf)
    if path then
      -- A buffer that is not loaded yet (one that was only added to the list) has no filetype: go by the name.
      local filetype = vim.bo[buf].filetype
      if filetype == '' then
        filetype = vim.filetype.match({ filename = path }) or ''
      end
      table.insert(tabs, {
        uri = vim.uri_from_fname(path),
        isActive = path == active,
        label = vim.fn.fnamemodify(path, ':t'),
        languageId = M.language_id(filetype),
        isDirty = vim.bo[buf].modified,
      })
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

return M
