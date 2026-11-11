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

check(
  'the environment says where the server listens, for it and for the adapter',
  transport.default_socket_path({ socket = '/somewhere/else', xdg_runtime_dir = '/run/user/1000', uid = 1000 }),
  '/somewhere/else'
)
check(
  'an empty value of the variable counts as unset',
  transport.default_socket_path({ socket = '', xdg_runtime_dir = '/run/user/1000', uid = 1000 }),
  '/run/user/1000/claude-ide-server/adapter.sock'
)

-- Where the server listens, when only the server can say (Windows): what its `--print-socket` prints. A program that prints
-- one line stands for it (`echo` prints its arguments, `--print-socket` among them); these need a shell, so not on Windows.
if vim.fn.has('win32') == 0 then
  check('the server says where it listens', { transport.ask_server('echo') }, { '--print-socket' })
  local missing, why = transport.ask_server('claude-ide-no-such-program')
  check('a server that is not there is said so', { missing, why ~= nil and why:find('was not found', 1, true) ~= nil }, { nil, true })
  local refused, failed = transport.ask_server('false')
  check('a server that fails is said so', { refused, failed ~= nil and failed:find('ended with code 1', 1, true) ~= nil }, { nil, true })
  local silent, nothing = transport.ask_server('true')
  check('a server that says nothing is said so', { silent, nothing ~= nil and nothing:find('printed nothing', 1, true) ~= nil }, { nil, true })
end

-- The wait before the next attempt doubles, up to a maximum.
local reconnect = { initial_ms = 500, max_ms = 10000 }
local delays = {}
for attempts = 0, 6 do
  table.insert(delays, Session.delay(attempts, reconnect))
end
check('backoff', delays, { 500, 1000, 2000, 4000, 8000, 10000, 10000 })
check('backoff does not overflow', Session.delay(10000, reconnect), 10000)
-- After the adapter has started the server, the first retry is soon, and never longer than the usual wait.
check('the first retry after a start', Session.delay_after_start(reconnect), 100)
check('and not longer than the usual first wait', Session.delay_after_start({ initial_ms = 40, max_ms = 80 }), 40)

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
check('the requests that are answered', declared, {
  'checkDocumentDirty',
  'closeAllDiffs',
  'closeDiff',
  'getCurrentSelection',
  'getDiagnostics',
  'getLatestSelection',
  'getOpenEditors',
  'openDiff',
  'openFile',
  'saveDocument',
})

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
check('and the tab says where it is and what it holds', {
  active[1].fileName == path,
  active[1].lineCount,
  active[1].isUntitled,
  active[1].isPinned,
  active[1].isPreview,
}, { true, 4, false, false, false })
check('the active tab has the selection', active[1].selection ~= nil and active[1].selection.isReversed == false, true)
check('and it is that file', active[1] and active[1].uri, vim.uri_from_fname(path))

-- A diagnostic: the columns are UTF-16 code units, and the severity has a name.
local lines = { 'aé😀b', 'second' }
local function line_of(lnum)
  return lines[lnum + 1] or ''
end
check(
  'a diagnostic after an emoji',
  methods.to_server_diagnostic(
    { lnum = 0, col = 7, end_lnum = 1, end_col = 3, severity = vim.diagnostic.severity.WARN, message = 'm', source = 's' },
    line_of
  ),
  { message = 'm', severity = 'Warning', source = 's', range = { start = { line = 0, character = 4 }, ['end'] = { line = 1, character = 3 } } }
)
check(
  'a diagnostic without an end or a source is empty at its start',
  methods.to_server_diagnostic({ lnum = 1, col = 2, severity = vim.diagnostic.severity.ERROR, message = 'x' }, line_of),
  { message = 'x', severity = 'Error', range = { start = { line = 1, character = 2 }, ['end'] = { line = 1, character = 2 } } }
)
check(
  'the severities',
  vim.tbl_map(function(severity)
    return methods.to_server_diagnostic({ lnum = 0, col = 0, severity = severity, message = '' }, line_of).severity
  end, { 1, 2, 3, 4 }),
  { 'Error', 'Warning', 'Information', 'Hint' }
)

-- Looking for the text that `openFile` is to select: where it is, in bytes, whatever the lines are.
local actions = require('claude_ide.actions')
local text_lines = { 'local a = 1', 'héllo wörld', 'print(a)' }
local function found(start_text, end_text, to_end)
  local match, start, last = actions.find_text(text_lines, start_text, end_text, to_end or false)
  return { match, start, last }
end
check('a text on a line', found('a = 1'), { 'text', { 0, 6 }, { 0, 10 } })
check('a text that is not there', found('nope'), { 'not_found' })
check('from a text to another', found('local', 'wörld'), { 'range', { 0, 0 }, { 1, 12 } })
check('the end text is not there: only the place', found('héllo', 'xyz'), { 'positioned', { 1, 0 } })
-- The last character is named by where it starts: `é` is two bytes, from column 1.
check('a text over two lines', found('1\nhé'), { 'text', { 0, 10 }, { 1, 1 } })
check('a range that ends inside a line', found('local', 'héllo'), { 'range', { 0, 0 }, { 1, 5 } })
check('a range extended to the end of the line', found('local', 'héllo', true), { 'range', { 0, 0 }, { 1, 12 } })
check('the offset of the start of a line', { actions.position_at(text_lines, 12) }, { 1, 0 })

-- The files of the buffers: dirty, saved, not open, and opened.
local dir = vim.fn.tempname()
vim.fn.mkdir(dir, 'p')
local file = vim.fn.resolve(dir) .. '/doc.lua'
vim.fn.writefile({ 'one', 'two', 'three' }, file)
check('a file that is not open', actions.checkDocumentDirty({ filePath = file }), { success = false })
check('opening a file that is not there fails', select(1, actions.openFile({ filePath = dir .. '/nope.lua' })), nil)
check('a directory is not a file that can be opened', select(1, actions.openFile({ filePath = dir })), nil)
local opened = actions.openFile({ filePath = file, startText = 'two' })
check('the file is opened', {
  opened.filePath == file,
  opened.languageId,
  opened.lineCount,
  opened.isDirty,
  opened.isUntitled,
  opened.selection,
}, { true, 'lua', 4, false, false, 'text' }) -- three lines and the empty one after the final newline
check('an open file that has no changes', actions.checkDocumentDirty({ filePath = file }), { isDirty = false, isUntitled = false })
vim.api.nvim_buf_set_lines(vim.fn.bufnr(file), 0, 1, false, { 'ONE' })
check('an open file that has changes', actions.checkDocumentDirty({ filePath = file }), { isDirty = true, isUntitled = false })
check('saving it', actions.saveDocument({ filePath = file }), { saved = true })
check('what was written', vim.fn.readfile(file)[1], 'ONE')
check('saving it again: nothing to write', actions.saveDocument({ filePath = file }), { saved = false })
-- With the focus in a terminal and no window that shows a file, a file gets a window of its own and the terminal stays.
vim.cmd('only')
vim.cmd.enew()
vim.cmd.terminal()
local terminal_buf = vim.api.nvim_get_current_buf()
local second = vim.fn.resolve(dir) .. '/second.lua'
vim.fn.writefile({ 'x' }, second)
local before = #vim.api.nvim_tabpage_list_wins(0)
actions.openFile({ filePath = second })
check('a window was added for the file', #vim.api.nvim_tabpage_list_wins(0), before + 1)
check('and the terminal is still shown', vim.fn.bufwinid(terminal_buf) ~= -1, true)
check('saving a file that is not open', actions.saveDocument({ filePath = dir .. '/other.lua' }), { success = false })

-- Diagnostics of a file that was written by someone else (Claude Code, say): the buffer is read again, and the diagnostics of what
-- is on the disk now are waited for, since whatever makes them (here a linter that answers a little after the file is read)
-- is not quick. A file that was not changed is answered at once.
local lint_file = vim.fn.resolve(dir) .. '/lint.lua'
vim.fn.writefile({ 'first' }, lint_file)
vim.cmd.edit(lint_file)
local lint_buf = vim.fn.bufnr(lint_file)
local namespace = vim.api.nvim_create_namespace('fake-linter')
vim.api.nvim_create_autocmd('BufReadPost', {
  buffer = lint_buf,
  callback = function(args)
    local text = vim.api.nvim_buf_get_lines(args.buf, 0, 1, false)[1]
    vim.defer_fn(function()
      vim.diagnostic.set(namespace, args.buf, { { lnum = 0, col = 0, message = 'about: ' .. text } })
    end, 120)
  end,
})
methods.wait = { first_ms = 750, quiet_ms = 150, cap_ms = 1500, cap_hidden_ms = 1000 }
local function diagnostics_of_uri(uri)
  local box = {}
  local started = vim.uv.now()
  local co = coroutine.create(function()
    box.result = methods.handlers.getDiagnostics({ uri = uri })
  end)
  assert(coroutine.resume(co))
  vim.wait(3000, function()
    return box.result ~= nil
  end, 10)
  return box.result, vim.uv.now() - started
end
local lint_uri = vim.uri_from_fname(lint_file)
local unchanged, quick = diagnostics_of_uri(lint_uri)
check('a file that was not changed is answered at once', quick < 100, true)
check('and has none yet', #unchanged[1].diagnostics, 0)
vim.fn.writefile({ 'second' }, lint_file) -- written by someone else
local fresh, waited = diagnostics_of_uri(lint_uri)
check('the diagnostics of the text that is on the disk now', fresh[1].diagnostics[1].message, 'about: second')
check('which were waited for', waited >= 100, true)
check('the buffer holds what is on the disk', vim.api.nvim_buf_get_lines(lint_buf, 0, -1, false), { 'second' })

-- A buffer with changes that were not written is the user's: not read again, and nothing is waited for.
vim.api.nvim_buf_set_lines(lint_buf, 0, -1, false, { 'typed' })
vim.fn.writefile({ 'third' }, lint_file)
local kept, quick_again = diagnostics_of_uri(lint_uri)
check('a buffer with changes is left alone', vim.api.nvim_buf_get_lines(lint_buf, 0, -1, false), { 'typed' })
check('and is not waited for', quick_again < 100, true)
vim.cmd('bwipeout! ' .. lint_buf)

-- A file that gets no diagnostics at all is given up on after the first wait, not the whole cap.
local silent_file = vim.fn.resolve(dir) .. '/silent.lua'
vim.fn.writefile({ 'a' }, silent_file)
vim.cmd.edit(silent_file)
vim.fn.writefile({ 'b' }, silent_file)
methods.wait = { first_ms = 200, quiet_ms = 50, cap_ms = 1500, cap_hidden_ms = 1000 }
local none, gave_up = diagnostics_of_uri(vim.uri_from_fname(silent_file))
check('no diagnostics come: it ends with none', #none[1].diagnostics, 0)
check('after the first wait', gave_up >= 180 and gave_up < 600, true)
methods.wait = false
local _, not_waiting = (function()
  vim.fn.writefile({ 'c' }, silent_file)
  return diagnostics_of_uri(vim.uri_from_fname(silent_file))
end)()
check('with `false` it does not wait', not_waiting < 100, true)
methods.wait = { first_ms = 750, quiet_ms = 150, cap_ms = 1500, cap_hidden_ms = 1000 }
vim.cmd('bwipeout! ' .. vim.fn.bufnr(silent_file))

-- A file that is asked for by a path through a link: it is found, and answered under the uri that it was asked for (Claude Code
-- compares that uri with the path of the file that it edited, and does not resolve links).
local real_dir = vim.fn.resolve(dir) .. '/real'
vim.fn.mkdir(real_dir, 'p')
local link_dir = vim.fn.resolve(dir) .. '/link'
vim.uv.fs_symlink(real_dir, link_dir)
local linked_real = real_dir .. '/linked.lua'
vim.fn.writefile({ 'x' }, linked_real)
vim.cmd.edit(linked_real)
vim.diagnostic.set(vim.api.nvim_create_namespace('link'), vim.fn.bufnr(linked_real), { { lnum = 0, col = 0, message = 'via the link' } })
local via_link = 'file://' .. link_dir .. '/linked.lua'
local found = methods.handlers.getDiagnostics({ uri = via_link })
check('found by a path through a link', found[1] and found[1].diagnostics[1].message, 'via the link')
check('and answered under the uri that was asked for', found[1] and found[1].uri, via_link)
local unlinked = methods.handlers.getDiagnostics({ uri = vim.uri_from_fname(linked_real) })
check('by the real path too', unlinked[1] and unlinked[1].uri, vim.uri_from_fname(linked_real))
check('a file that is not open has none', methods.handlers.getDiagnostics({ uri = 'file://' .. dir .. '/never-opened.lua' }), {})
local everyone = methods.handlers.getDiagnostics()
check('without a uri, the uri of the real path', everyone[1] and everyone[1].uri, vim.uri_from_fname(linked_real))
vim.cmd('bwipeout! ' .. vim.fn.bufnr(linked_real))

-- Diffs. A request waits (a coroutine, as the requests of the server are), and the user decides in the tab that is shown.
local diff = require('claude_ide.diff')
local function open_diff(name, contents, id)
  local box = {}
  local co = coroutine.create(function()
    box.result = diff.openDiff({
      tabName = name,
      oldFilePath = file,
      newFilePath = file,
      newFileContents = contents,
      diffId = id,
    })
  end)
  assert(coroutine.resume(co))
  return box
end
local function wait_for(box)
  vim.wait(2000, function()
    return box.result ~= nil
  end, 10)
  return box.result
end
local function tabs()
  return #vim.api.nvim_list_tabpages()
end
local function buffer_named(name)
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_get_name(buf):find('[claude] ' .. name, 1, true) and vim.bo[buf].buftype == 'acwrite' then
      return buf
    end
  end
end

diff.configure({})
vim.cmd('only')
local tabs_before = tabs()

-- The left side is the file as it is on the disk, unless the buffer has changes that were not written: a buffer that is
-- older than the file (written by someone else since) is not what the user has.
local on_disk = vim.fn.resolve(dir) .. '/on_disk.lua'
vim.fn.writefile({ 'old' }, on_disk)
vim.cmd.edit(on_disk)
vim.fn.writefile({ 'new, written by someone else' }, on_disk)
local function left_of(name)
  local box = {}
  local co = coroutine.create(function()
    box.result = diff.openDiff({ tabName = name, oldFilePath = on_disk, newFilePath = on_disk, newFileContents = 'x\n', diffId = 99 })
  end)
  assert(coroutine.resume(co))
  local lines
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_get_name(buf):find('[claude] ' .. name .. ' (now)', 1, true) then
      lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
    end
  end
  diff.reject(name)
  wait_for(box)
  return lines
end
check('a buffer without changes is not used: the file is', left_of('disk'), { 'new, written by someone else' })
vim.api.nvim_buf_set_lines(vim.fn.bufnr(on_disk), 0, -1, false, { 'typed by the user, not written' })
check('a buffer with changes is what the user has', left_of('typed'), { 'typed by the user, not written' })
vim.cmd('bwipeout! ' .. vim.fn.bufnr(on_disk))

-- Accepted by writing the right side, after it was edited: what is given back is what the right side holds then.
local box = open_diff('edit-1', 'TWO\nthree\n', 7)
check('a diff is shown in a tab page of its own', tabs(), tabs_before + 1)
check('and it waits', box.result, nil)
check('one diff waits', diff.count(), 1)
local right = buffer_named('edit-1')
check('the right side holds what is proposed', vim.api.nvim_buf_get_lines(right, 0, -1, false), { 'TWO', 'three' })
check('and ends with a newline', vim.bo[right].eol, true)
vim.api.nvim_buf_set_lines(right, 0, 1, false, { 'TWO (edited)' })
vim.cmd('write')
check('writing accepts it, with the contents as they are', wait_for(box), { accepted = true, contents = 'TWO (edited)\nthree\n' })
check('and the tab page is closed', tabs(), tabs_before)
check('nothing waits any more', diff.count(), 0)

-- Rejected by a command.
box = open_diff('edit-2', 'x\n', 8)
check('a command rejects it', { diff.reject() }, { true })
check('rejected', wait_for(box), { accepted = false })
check('and the tab page is closed again', tabs(), tabs_before)

-- Rejected by closing the tab page.
box = open_diff('edit-3', 'x\n', 9)
vim.cmd('tabclose')
check('closing the tab page rejects it', wait_for(box), { accepted = false })

-- A diff of the same name replaces the one before it, which is rejected; the new one is shown.
local first = open_diff('same', 'one\n', 10)
local second = open_diff('same', 'two\n', 11)
check('the older diff is rejected', wait_for(first), { accepted = false })
check('the newer one waits', second.result, nil)
check('and is the only one', diff.count(), 1)

-- Closed from Claude Code: with the number of a diff that is gone, a newer diff of the same name is left alone.
check('another number leaves it', diff.closeDiff({ tabName = 'same', diffId = 10 }), { closed = false })
check('still waiting', second.result, nil)
check('its own number closes it', diff.closeDiff({ tabName = 'same', diffId = 11 }), { closed = true })
check('closed means rejected', wait_for(second), { accepted = false })
check('a name that is not there', diff.closeDiff({ tabName = 'nothing' }), { closed = false })

-- All of them.
local a, b = open_diff('a', 'a\n', 12), open_diff('b', 'b\n', 13)
check('how many were closed', diff.closeAllDiffs(), { closed = 2 })
check('both are rejected', { wait_for(a).accepted, wait_for(b).accepted }, { false, false })
check('no tab page is left over', tabs(), tabs_before)

-- A provider of the configuration's own: it is asked to show and to close, and decides when it wants.
local calls = {}
diff.configure({
  provider = {
    open = function(spec, finish)
      table.insert(calls, 'open ' .. spec.tab_name .. ' ' .. spec.new_contents)
      vim.schedule(function()
        finish(true, 'by the provider')
      end)
      return { name = spec.tab_name }
    end,
    close = function(handle)
      table.insert(calls, 'close ' .. handle.name)
    end,
  },
})
box = open_diff('own', 'proposed', 14)
check('the provider decides', wait_for(box), { accepted = true, contents = 'by the provider' })
check('and is asked to close', calls, { 'open own proposed', 'close own' })
check('without a tab page of this adapter', tabs(), tabs_before)
-- A provider that gives nothing back is still asked to close.
local closed_without_handle = 0
diff.configure({
  provider = {
    open = function(_, finish)
      vim.schedule(function()
        finish(false)
      end)
    end,
    close = function()
      closed_without_handle = closed_without_handle + 1
    end,
  },
})
box = open_diff('no handle', 'x', 15)
check('a provider that returns no handle', wait_for(box), { accepted = false })
vim.wait(100, function()
  return false
end, 10)
check('is still asked to close', closed_without_handle, 1)
diff.configure({})

if failures > 0 then
  io.stderr:write(('%d checks failed\n'):format(failures))
  os.exit(1)
end
print('unit: ok')
