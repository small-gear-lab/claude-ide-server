--- The connection to the server: connect, `register`, answer the server's requests, and connect again when it is gone.
---
--- States: `stopped` (not asked to run), `waiting` (no connection; a new attempt is scheduled), `connecting` (a
--- connection is open, `register` is not answered yet), `registered`.
local server = require('claude_ide.server')
local transport = require('claude_ide.transport')

local PROTOCOL_VERSION = 1

---@class claude_ide.Session
---@field opts claude_ide.Options
---@field state 'stopped'|'waiting'|'connecting'|'registered'
---@field port integer? The port of the workspace, once registered.
---@field attempts integer Connection attempts since the last success.
---@field last_error string? Why the last attempt, or the last connection, failed.
local Session = {}
Session.__index = Session

--- How long to wait before the next attempt: doubles from `initial_ms`, up to `max_ms`.
---@param attempts integer Attempts so far (0 for the first retry).
---@param reconnect { initial_ms: integer, max_ms: integer }
---@return integer
function Session.delay(attempts, reconnect)
  local delay = reconnect.initial_ms * 2 ^ math.min(attempts, 30)
  return math.floor(math.min(delay, reconnect.max_ms))
end

---@param opts claude_ide.Options
---@return claude_ide.Session
function Session.new(opts)
  return setmetatable({
    opts = opts,
    state = 'stopped',
    attempts = 0,
    -- Told apart, so that what a connection that has been given up says late is ignored.
    generation = 0,
  }, Session)
end

function Session:start()
  if self.state ~= 'stopped' then
    return
  end
  self.state = 'waiting'
  self:_attempt()
end

function Session:stop()
  self.generation = self.generation + 1
  self:_gone()
  self:_drop_connection()
  self.state = 'stopped'
  self.port = nil
  -- Started again by the user: if there is still no server, one is started again too.
  self.started_server = false
end

---@return string
function Session:socket()
  return self.opts.socket or transport.socket_path()
end

--- Sends a notification to the server, if there is a registered connection. Returns whether it was sent.
---@param method string
---@param params table
---@return boolean
function Session:notify(method, params)
  if self.state ~= 'registered' or not self.client then
    return false
  end
  return self.client.notify(method, params)
end

function Session:_attempt()
  local generation = self.generation
  transport.probe(self:socket(), function(ok, err)
    if generation ~= self.generation then
      return
    end
    if ok then
      self:_open(generation)
      return
    end
    -- Nobody is listening. Start the server, once for as long as there is none, and then wait for it like for any other.
    if not self.started_server and self.opts.server.autostart and server.nobody_there(err) then
      self.started_server = true
      local problem = server.start(self.opts.server, self.opts.socket, function(code)
        self:_server_exited(code)
      end)
      if problem then
        self:_warn(problem)
      end
    end
    self:_lost(generation, err)
  end)
end

function Session:_open(generation)
  local dispatchers = {
    notification = function(method)
      -- The server is stopping, or dropping this workspace: it closes the connection too, and this is not waited for.
      if method == 'shutdown' then
        vim.schedule(function()
          self:_lost(generation, 'the server is shutting down')
        end)
      end
    end,
    server_request = function(method, params)
      return self:_serve(method, params)
    end,
    on_exit = function()
      vim.schedule(function()
        self:_lost(generation, 'the server closed the connection')
      end)
    end,
  }
  local ok, client = pcall(vim.lsp.rpc.connect(self:socket()), dispatchers)
  if not ok then
    self:_lost(generation, tostring(client))
    return
  end
  self.client = client
  self.state = 'connecting'

  -- A server that does not answer is as good as one that is not there.
  local timer = vim.defer_fn(function()
    if generation == self.generation and self.state == 'connecting' then
      self:_lost(generation, 'no answer to register')
    end
  end, self.opts.register_timeout_ms)

  local sent = client.request('register', self:_registration(), function(err, result)
    -- The answer may arrive where the editor's functions cannot be called.
    vim.schedule(function()
      -- Closed once, by whoever gets here first: the timer closes itself after it has fired.
      if not timer:is_closing() then
        timer:stop()
        timer:close()
      end
      if err then
        -- A server that refuses is not a server that is not there yet: trying again changes nothing until one of the
        -- two is changed, so the user is told, once. (Also when the connection was seen to close first: the server
        -- closes it after refusing, and which of the two is noticed first is not in our hands.)
        local reason = ('register was refused: %s'):format(err.message or vim.inspect(err))
        if self.state ~= 'stopped' then
          self.last_error = reason
          self:_warn(reason)
        end
        self:_lost(generation, reason)
        return
      end
      if generation ~= self.generation then
        return
      end
      self.port = result and result.port
      self.state = 'registered'
      self.attempts = 0
      self.last_error = nil
      self.warned = nil
      -- If this server goes away, the next one is started by whoever finds nobody there.
      self.started_server = false
      if self.opts.on_registered then
        self.opts.on_registered(self)
      end
    end)
  end)
  if not sent then
    self:_lost(generation, 'could not send register')
  end
end

---@return table
function Session:_registration()
  local folders = self.opts.workspace_folders()
  if #folders == 0 then
    folders = { vim.uv.cwd() }
  end
  return {
    protocolVersion = PROTOCOL_VERSION,
    ideName = self.opts.ide_name,
    pid = vim.uv.os_getpid(),
    workspaceFolders = folders,
    methods = vim.tbl_keys(self.opts.methods),
  }
end

--- What the server asks for. It is answered from `opts.methods`; anything else is an error that says so.
function Session:_serve(method, params)
  local handler = self.opts.methods[method]
  if not handler then
    return nil, vim.lsp.rpc.rpc_response_error(vim.lsp.protocol.ErrorCodes.MethodNotFound, 'method not found: ' .. method)
  end
  return handler(params)
end

--- The server that this adapter started has ended. Its output is not kept, so the exit code is all that there is to
--- say. A server that ends with 0 was stopped as asked; with 1 it may have lost the race to another one (which is
--- then the server that is connected to) or may not have been able to start, which `:ClaudeIde status` shows if it
--- stays unreachable; with anything else, it did not understand its command line, which the user should hear.
---@param code integer
function Session:_server_exited(code)
  vim.schedule(function()
    if code == 0 or self.state == 'registered' or self.state == 'stopped' then
      return
    end
    local reason = ('the server that was started exited with code %d (run it by hand to see why)'):format(code)
    self.last_error = reason
    if code >= 2 then
      self:_warn(reason)
    end
  end)
end

--- Tells the user of a problem that retrying does not solve, once for each reason.
---@param reason string
function Session:_warn(reason)
  if self.warned ~= reason then
    self.warned = reason
    vim.notify('claude-ide: ' .. reason, vim.log.levels.WARN)
  end
end

--- What was started for a server that is not there any more is of no use to anyone: told to `opts.on_gone`.
function Session:_gone()
  if self.opts.on_gone then
    pcall(self.opts.on_gone)
  end
end

--- The connection is gone, or never was: schedule the next attempt.
function Session:_lost(generation, reason)
  if generation ~= self.generation or self.state == 'stopped' then
    return
  end
  self.generation = self.generation + 1
  self:_gone()
  self:_drop_connection()
  self.port = nil
  self.last_error = reason
  self.state = 'waiting'
  local delay = Session.delay(self.attempts, self.opts.reconnect)
  self.attempts = self.attempts + 1
  local next_generation = self.generation
  vim.defer_fn(function()
    if next_generation == self.generation and self.state == 'waiting' then
      self:_attempt()
    end
  end, delay)
end

function Session:_drop_connection()
  local client = self.client
  self.client = nil
  if client and not client.is_closing() then
    client.terminate()
  end
end

return Session
