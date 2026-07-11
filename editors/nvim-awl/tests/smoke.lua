local source = debug.getinfo(1, 'S').source:sub(2)
local plugin = vim.fs.dirname(vim.fs.dirname(source))
local repo = vim.env.AION_ROOT or vim.fs.dirname(vim.fs.dirname(plugin))
local parser_runtime = assert(vim.env.AWL_PARSER_RUNTIME, 'AWL_PARSER_RUNTIME is required')

vim.opt.runtimepath:prepend(parser_runtime)
vim.opt.runtimepath:prepend(plugin)
vim.cmd('runtime plugin/awl.lua')
require('awl').setup({ lsp = false })
vim.cmd('filetype plugin on')

local sample = repo .. '/docs/design/aion-authoring/awl/examples/rev2/awl_hello.awl'
vim.cmd.edit(vim.fn.fnameescape(sample))

assert(vim.bo.filetype == 'awl', 'expected filetype=awl, got ' .. vim.bo.filetype)
local parser = vim.treesitter.get_parser(0, 'awl')
assert(parser, 'vim.treesitter.get_parser returned nil')
assert(#parser:parse() > 0, 'AWL parser returned no trees')

for _, name in ipairs({ 'highlights', 'folds', 'indents' }) do
  local path = plugin .. '/queries/awl/' .. name .. '.scm'
  local lines = vim.fn.readfile(path)
  assert(vim.treesitter.query.parse('awl', table.concat(lines, '\n')), 'query did not parse: ' .. name)
end

assert(vim.treesitter.query.get('awl', 'highlights'), 'runtime highlights query was not found')
assert(vim.lsp.config.awl.cmd[1] == 'aion', 'AWL LSP runtime config was not found')

local awl = require('awl')
local current_configs = {}
package.loaded['nvim-treesitter.parsers'] = current_configs
assert(awl.register_parser(), 'current nvim-treesitter parser registration failed')
assert(current_configs.awl.install_info.url == 'https://github.com/tomWhiting/tree-sitter-awl.git')

local legacy_configs = {}
package.loaded['nvim-treesitter.parsers'] = {
  get_parser_configs = function()
    return legacy_configs
  end,
}
awl.setup({
  lsp = false,
  parser = {
    install_info = {
      path = repo,
      location = 'tools/tree-sitter-awl',
    },
  },
})
assert(legacy_configs.awl.install_info.path == repo, 'legacy local parser path was not registered')
assert(legacy_configs.awl.install_info.url == nil, 'local parser config retained the default URL')

print('nvim-awl smoke: filetype, parser, queries, parser registration, and LSP config OK')
