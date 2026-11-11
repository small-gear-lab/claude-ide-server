--- Where the server's adapter socket is, and whether anyone is listening on it.
local M = {}

--- The path that the server uses when it is not given one (`workspace::socket_path` and `lock::runtime_dir`):
--- `adapter.sock` in `$XDG_RUNTIME_DIR/claude-ide-server`, or else in a directory of the user under the temporary one.
---@param env { xdg_runtime_dir: string?, tmpdir: string?, uid: integer? }
---@return string
function M.default_socket_path(env)
  local function set(value)
    return value ~= nil and value ~= ''
  end
  if set(env.xdg_runtime_dir) then
    return env.xdg_runtime_dir .. '/claude-ide-server/adapter.sock'
  end
  local tmpdir = set(env.tmpdir) and env.tmpdir or '/tmp'
  return string.format('%s/claude-ide-server-%d/adapter.sock', tmpdir, env.uid or 0)
end

--- The path for this Neovim's environment.
---@return string
function M.socket_path()
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
