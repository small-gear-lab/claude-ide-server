-- The parts of the adapter that need no server. Run: nvim --headless -u NONE -l editors/nvim/tests/unit.lua
-- (from the repository root). Exits non-zero if a check fails.
local root = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p:h:h')
vim.opt.runtimepath:prepend(root)

local failures = 0
local function check(name, actual, expected)
  if not vim.deep_equal(actual, expected) then
    failures = failures + 1
    io.stderr:write(('FAIL %s\n  expected: %s\n  actual:   %s\n'):format(name, vim.inspect(expected), vim.inspect(actual)))
  end
end

local transport = require('claude_ide.transport')
local Session = require('claude_ide.session')

-- The socket path is the one that the server computes (workspace::socket_path, lock::runtime_dir).
check(
  'socket in the runtime directory',
  transport.default_socket_path({ xdg_runtime_dir = '/run/user/1000', tmpdir = '/tmp', uid = 1000 }),
  '/run/user/1000/claude-ide-server/adapter.sock'
)
check(
  'an empty runtime directory counts as unset',
  transport.default_socket_path({ xdg_runtime_dir = '', tmpdir = '/var/tmp', uid = 7 }),
  '/var/tmp/claude-ide-server-7/adapter.sock'
)
check(
  'no runtime directory and no temporary directory',
  transport.default_socket_path({ uid = 501 }),
  '/tmp/claude-ide-server-501/adapter.sock'
)

-- The wait before the next attempt doubles, up to a maximum.
local reconnect = { initial_ms = 500, max_ms = 10000 }
local delays = {}
for attempts = 0, 6 do
  table.insert(delays, Session.delay(attempts, reconnect))
end
check('backoff', delays, { 500, 1000, 2000, 4000, 8000, 10000, 10000 })
check('backoff does not overflow', Session.delay(10000, reconnect), 10000)

-- Columns are counted in UTF-16 code units, not bytes: `é` is two bytes and one unit, an emoji is four bytes and two.
local selection = require('claude_ide.selection')
check('utf16 of ascii', selection.utf16_col('abc', 2), 2)
check('utf16 after a two byte character', selection.utf16_col('aéb', 3), 2)
check('utf16 after an emoji', selection.utf16_col('a😀b', 5), 3)
check('utf16 past the end counts the whole line', selection.utf16_col('a😀', 99), 3)

local scratch = vim.fn.tempname() .. '.txt'
vim.cmd.edit(scratch)
vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'aé😀b', 'second line', '' })
local buf = vim.api.nvim_get_current_buf()
-- What Neovim calls the file, which is the real path (on macOS the temporary directory is under a link: /var is
-- /private/var), and so is what the adapter reports.
local path = vim.fn.fnamemodify(vim.api.nvim_buf_get_name(buf), ':p')
local function pos(lnum, col)
  return { buf, lnum, col, 0 }
end
local function range(start_line, start_char, end_line, end_char)
  return { start = { line = start_line, character = start_char }, ['end'] = { line = end_line, character = end_char } }
end

check(
  'a cursor is an empty range',
  selection.current(buf, { mode = 'n', visual = pos(1, 1), cursor = pos(1, 8) }),
  { filePath = path, text = '', selection = range(0, 4, 0, 4) }
)
check(
  'characterwise: from é to the end of the emoji',
  selection.current(buf, { mode = 'v', visual = pos(1, 2), cursor = pos(1, 4) }),
  { filePath = path, text = 'é😀', selection = range(0, 1, 0, 4) }
)
check(
  'linewise: whole lines, to the end of the last',
  selection.current(buf, { mode = 'V', visual = pos(1, 1), cursor = pos(2, 3) }),
  { filePath = path, text = 'aé😀b\nsecond line', selection = range(0, 0, 1, 11) }
)
check(
  'backwards is the same as forwards',
  selection.current(buf, { mode = 'v', visual = pos(1, 4), cursor = pos(1, 2) }),
  { filePath = path, text = 'é😀', selection = range(0, 1, 0, 4) }
)
check(
  'an empty line',
  selection.current(buf, { mode = 'n', visual = pos(3, 1), cursor = pos(3, 1) }),
  { filePath = path, text = '', selection = range(2, 0, 2, 0) }
)
vim.cmd.enew()
check('a buffer without a name is not a file', selection.current(vim.api.nvim_get_current_buf(), { mode = 'n', visual = pos(1, 1), cursor = pos(1, 1) }), nil)
vim.cmd.terminal()
check('a terminal is not a file', selection.current(vim.api.nvim_get_current_buf(), { mode = 'n', visual = pos(1, 1), cursor = pos(1, 1) }), nil)

-- Starting the server: only when nobody is there, and only with the socket that the adapter was given.
local server = require('claude_ide.server')
check('no one at the socket', server.nobody_there('ENOENT: no such file or directory'), true)
check('a refused connection', server.nobody_there('ECONNREFUSED: connection refused'), true)
check('a refusal of permission is not nobody', server.nobody_there('EACCES: permission denied'), false)
check('no error is not nobody', server.nobody_there(nil), false)
local opts = { command = 'claude-ide-server', args = { '--idle-timeout', '300' } }
check('command line', server.command_line(opts, nil), { 'claude-ide-server', '--idle-timeout', '300' })
check(
  'command line with a socket',
  server.command_line(opts, '/run/a.sock'),
  { 'claude-ide-server', '--idle-timeout', '300', '--socket', '/run/a.sock' }
)
check('an executable that is not there is said so', server.start({ command = 'no-such-claude-ide-server', args = {} }) ~= nil, true)

-- What the server asks for. A terminal has no selection and is not an editor: what is answered is about the file that
-- the user was in.
local methods = require('claude_ide.methods')
check('a filetype that differs from the language id', methods.language_id('sh'), 'shellscript')
check('a filetype that is the language id', methods.language_id('lua'), 'lua')
check('no filetype', methods.language_id(''), 'plaintext')
-- `register` declares the names in this table: nothing else may be in it.
local declared = vim.tbl_keys(methods.handlers)
table.sort(declared)
check('the requests that are answered', declared, { 'getCurrentSelection', 'getLatestSelection', 'getOpenEditors' })

vim.cmd.edit(scratch)
local in_file = methods.handlers.getCurrentSelection()
check('the selection in a file', in_file.filePath, path)
vim.cmd.enew()
vim.cmd.terminal()
check('in a terminal the last selection is what there is', methods.handlers.getCurrentSelection().filePath, path)
local active = vim.tbl_filter(function(tab)
  return tab.isActive
end, methods.handlers.getOpenEditors().tabs)
check('in a terminal the file that the user was in is the active one', #active, 1)
check('and it is that file', active[1] and active[1].uri, vim.uri_from_fname(path))

if failures > 0 then
  io.stderr:write(('%d checks failed\n'):format(failures))
  os.exit(1)
end
print('unit: ok')
