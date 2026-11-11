--- Starting `claude-ide-server` when nobody is listening.
---
--- A user should not have to start the server. Under socket activation (systemd, see `contrib/`) the supervisor starts
--- it at the first connection, and the adapter never gets here, because the connection works. Without that, the adapter
--- does the same: a connection that finds nobody is the cue to start the server, and the server ends by itself when no
--- adapter has been there for a while (`--idle-timeout`). Several adapters may do it at the same moment: the server
--- refuses to start twice on one socket, so one of them wins and the others connect to it.
local M = {}

---@class claude_ide.ServerOptions
---@field autostart boolean Start the server when nobody is listening.
---@field command string The executable, looked up on `PATH` unless it is a path.
---@field args string[] Its arguments.

--- Whether a failed connection says that nobody is there (as opposed to a refusal of permission, or a path that is
--- not a socket, which starting a server would not change).
---@param err string? The error of the connection (`ENOENT: ...`).
---@return boolean
function M.nobody_there(err)
  return err ~= nil and (err:match('^ENOENT') ~= nil or err:match('^ECONNREFUSED') ~= nil)
end

--- The command line, with `--socket` when the adapter was told where the socket is.
---@param server claude_ide.ServerOptions
---@param socket string? The socket that the adapter was given, if it was.
---@return string[]
function M.command_line(server, socket)
  local line = { server.command }
  vim.list_extend(line, server.args)
  if socket then
    vim.list_extend(line, { '--socket', socket })
  end
  return line
end

--- Starts the server, detached: it is not a child of this Neovim, and goes on when Neovim ends. Returns nil, or why it
--- could not be started.
---@param server claude_ide.ServerOptions
---@param socket string?
---@param on_exit fun(code: integer)? Called with the exit code if the server ends while this Neovim is still running.
---@return string?
function M.start(server, socket, on_exit)
  if vim.fn.executable(server.command) ~= 1 then
    return ('%s was not found (is it on the PATH? `server.command` says which to run)'):format(server.command)
  end
  -- Its output is not wanted here, and not kept: a process that is detached must not hold a pipe to this Neovim,
  -- which would be gone when it writes its log. (Run it through your own wrapper, with `server.command`, to keep one.)
  local ok, err = pcall(vim.system, M.command_line(server, socket), {
    detach = true,
    stdout = false,
    stderr = false,
  }, function(result)
    if on_exit then
      on_exit(result.code)
    end
  end)
  if not ok then
    return ('could not start %s: %s'):format(server.command, tostring(err))
  end
  return nil
end

return M
