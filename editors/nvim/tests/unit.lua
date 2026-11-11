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

if failures > 0 then
  io.stderr:write(('%d checks failed\n'):format(failures))
  os.exit(1)
end
print('unit: ok')
