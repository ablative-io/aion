local M = {}

local defaults = {
  lsp = true,
  format_on_save = false,
  format_timeout_ms = 1000,
  parser = {
    install_info = {
      url = 'https://github.com/ablative-io/aion.git',
      location = 'tools/tree-sitter-awl',
    },
  },
}

local options = vim.deepcopy(defaults)
local format_group = vim.api.nvim_create_augroup('nvim_awl_format', { clear = true })
local attach_group = vim.api.nvim_create_augroup('nvim_awl_lsp_attach', { clear = true })

local function parser_configs()
  local ok, parsers = pcall(require, 'nvim-treesitter.parsers')
  if not ok then
    return nil
  end

  if type(parsers.get_parser_configs) == 'function' then
    return parsers.get_parser_configs()
  end

  return parsers
end

function M.register_parser()
  if options.parser == false then
    return false
  end

  local configs = parser_configs()
  if not configs then
    return false
  end

  configs.awl = {
    install_info = vim.deepcopy(options.parser.install_info),
    filetype = 'awl',
  }
  return true
end

local function enable_formatting(client, bufnr)
  if not options.format_on_save or client.name ~= 'awl' then
    return
  end
  if not client:supports_method('textDocument/formatting', bufnr) then
    return
  end

  vim.api.nvim_clear_autocmds({
    group = format_group,
    event = 'BufWritePre',
    buffer = bufnr,
  })
  vim.api.nvim_create_autocmd('BufWritePre', {
    group = format_group,
    buffer = bufnr,
    desc = 'Format AWL with aion awl lsp',
    callback = function()
      vim.lsp.buf.format({
        bufnr = bufnr,
        id = client.id,
        timeout_ms = options.format_timeout_ms,
      })
    end,
  })
end

vim.api.nvim_create_autocmd('User', {
  group = vim.api.nvim_create_augroup('nvim_awl_parser', { clear = true }),
  pattern = 'TSUpdate',
  desc = 'Register the AWL parser with nvim-treesitter',
  callback = M.register_parser,
})

vim.api.nvim_create_autocmd('LspAttach', {
  group = attach_group,
  desc = 'Configure AWL LSP formatting',
  callback = function(args)
    if vim.bo[args.buf].filetype ~= 'awl' then
      return
    end
    local client = vim.lsp.get_client_by_id(args.data.client_id)
    if client then
      enable_formatting(client, args.buf)
    end
  end,
})

function M.setup(opts)
  opts = opts or {}
  options = vim.tbl_deep_extend('force', vim.deepcopy(defaults), opts)
  if opts.parser and opts.parser.install_info then
    options.parser.install_info = vim.deepcopy(opts.parser.install_info)
  end

  M.register_parser()

  if vim.lsp and vim.lsp.enable then
    vim.lsp.enable('awl', options.lsp ~= false)
  end

  if not options.format_on_save then
    vim.api.nvim_clear_autocmds({ group = format_group })
  else
    for _, client in ipairs(vim.lsp.get_clients({ name = 'awl' })) do
      for bufnr in pairs(client.attached_buffers or {}) do
        enable_formatting(client, bufnr)
      end
    end
  end

  return M
end

return M
