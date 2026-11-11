-- `:ClaudeIde status|start|stop`. Nothing runs until `require('claude_ide').setup()` is called.
vim.api.nvim_create_user_command('ClaudeIde', function(command)
  local claude_ide = require('claude_ide')
  local action = command.fargs[1] or 'status'
  if action == 'start' then
    if not claude_ide.start() then
      vim.notify('claude-ide: this Neovim is not to register (should_register)', vim.log.levels.INFO)
    end
  elseif action == 'stop' then
    claude_ide.stop()
  elseif action == 'status' then
    local status = claude_ide.status()
    local lines = { ('claude-ide: %s'):format(status.state) }
    if status.port then
      table.insert(lines, ('port %d'):format(status.port))
    end
    if status.socket then
      table.insert(lines, ('socket %s'):format(status.socket))
    end
    if status.last_error then
      table.insert(lines, ('last problem: %s'):format(status.last_error))
    end
    vim.notify(table.concat(lines, '\n'), vim.log.levels.INFO)
  else
    vim.notify('claude-ide: unknown action ' .. action, vim.log.levels.ERROR)
  end
end, {
  nargs = '?',
  complete = function()
    return { 'status', 'start', 'stop' }
  end,
  desc = 'Claude Code /ide connection',
})

-- `:ClaudeIdeMention` mentions the current file to Claude Code; `:'<,'>ClaudeIdeMention` the lines that are selected.
vim.api.nvim_create_user_command('ClaudeIdeMention', function(command)
  local sent, why = require('claude_ide').mention(command.range > 0 and command.line1 or nil, command.range > 0 and command.line2 or nil)
  if not sent then
    vim.notify('claude-ide: ' .. why, vim.log.levels.WARN)
  end
end, {
  range = true,
  desc = 'Mention the file, or the lines, to Claude Code',
})

-- `:ClaudeIdeDiffAccept` and `:ClaudeIdeDiffReject` decide the diff that Claude Code proposes: the one of the current
-- buffer, or the only one that waits, or the one that is named.
for name, action in pairs({ ClaudeIdeDiffAccept = 'accept', ClaudeIdeDiffReject = 'reject' }) do
  vim.api.nvim_create_user_command(name, function(command)
    local ok, why = require('claude_ide.diff')[action](command.fargs[1])
    if not ok then
      vim.notify('claude-ide: ' .. why, vim.log.levels.WARN)
    end
  end, { nargs = '?', desc = action:sub(1, 1):upper() .. action:sub(2) .. ' the change that Claude Code proposes' })
end
