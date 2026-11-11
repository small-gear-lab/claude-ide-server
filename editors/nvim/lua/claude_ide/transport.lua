--- Where the server's adapter socket is, and whether anyone is listening on it.
local M = {}

--- The path that the server uses when it is not given one (`workspace::socket_path` and `lock::runtime_dir`):
--- `adapter.sock` in `$XDG_RUNTIME_DIR/claude-ide-server`, or else in a directory of the user under the temporary one.
---@param env { socket: string?, xdg_runtime_dir: string?, tmpdir: string?, uid: integer? }
---@return string
function M.default_socket_path(env)
  local function set(value)
    return value ~= nil and value ~= ''
  end
  -- `CLAUDE_IDE_SERVER_SOCKET` is where the server listens too, if it reads the same variable (it does): one place, set once.
  if set(env.socket) then
    return env.socket
  end
  if set(env.xdg_runtime_dir) then
    return env.xdg_runtime_dir .. '/claude-ide-server/adapter.sock'
  end
  local tmpdir = set(env.tmpdir) and env.tmpdir or '/tmp'
  return string.format('%s/claude-ide-server-%d/adapter.sock', tmpdir, env.uid or 0)
end

--- Asks the server executable where the adapters connect (`claude-ide-server --print-socket`): for a system where the
--- default is not something that this program can work out as the server does. On Windows it is the named pipe with the
--- security identifier of the user in its name, and Neovim does not give a SID.
---@param command string The executable, looked up on `PATH` unless it is a path.
---@return string? path
---@return string? why Why there is none.
function M.ask_server(command)
  if vim.fn.executable(command) ~= 1 then
    return nil,
      ('%s was not found, so where the server listens is not known (is it on the PATH? `server.command` says which to run, and `socket` says where it listens)'):format(
        command
      )
  end
  local ok, result = pcall(function()
    return vim.system({ command, '--print-socket' }, { text = true }):wait(5000)
  end)
  if not ok then
    return nil, ('could not run %s: %s'):format(command, tostring(result))
  end
  if result.code ~= 0 then
    return nil, ('%s --print-socket ended with code %s'):format(command, tostring(result.code))
  end
  local path = vim.trim(result.stdout or '')
  if path == '' then
    return nil, ('%s --print-socket printed nothing'):format(command)
  end
  return path
end

--- Where the server listens by default, for this Neovim's environment: what `CLAUDE_IDE_SERVER_SOCKET` says, or the path that
--- the server works out (see `default_socket_path`), and on Windows what the server says.
---@param server_command string The executable of the server, for a system that has to ask it.
---@return string? path
---@return string? why Why there is none.
function M.socket_path(server_command)
  local set = vim.env.CLAUDE_IDE_SERVER_SOCKET
  if set ~= nil and set ~= '' then
    return set
  end
  if vim.fn.has('win32') == 1 then
    return M.ask_server(server_command)
  end
  local passwd = vim.uv.os_get_passwd()
  return M.default_socket_path({
    xdg_runtime_dir = vim.env.XDG_RUNTIME_DIR,
    tmpdir = vim.env.TMPDIR,
    uid = passwd and passwd.uid or 0,
  })
end

--- Tries a connection and closes it again, and says how that went. `vim.lsp.rpc.connect` reports a connection that
--- fails only with a warning message, and calls nothing: an adapter that retries could neither tell nor stay quiet.
--- (The server ignores a connection that says nothing and closes.)
---@param path string
---@param callback fun(ok: boolean, err: string?) Called on the main loop.
function M.probe(path, callback)
  local pipe = vim.uv.new_pipe(false)
  if not pipe then
    callback(false, 'could not create a pipe')
    return
  end
  local request, failure = pipe:connect(path, function(err)
    pipe:close()
    vim.schedule(function()
      callback(err == nil, err)
    end)
  end)
  -- A connection that could not even be started calls nothing back: say so, or the caller would wait for ever.
  if not request then
    pipe:close()
    vim.schedule(function()
      callback(false, tostring(failure))
    end)
  end
end

return M
