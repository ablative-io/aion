vim.bo.commentstring = '// %s'
vim.wo.foldexpr = 'v:lua.vim.treesitter.foldexpr()'

pcall(vim.treesitter.start, 0, 'awl')

local ok, treesitter = pcall(require, 'nvim-treesitter')
if ok and type(treesitter.indentexpr) == 'function' then
  vim.bo.indentexpr = "v:lua.require'nvim-treesitter'.indentexpr()"
end

vim.b.undo_ftplugin = table.concat({
  vim.b.undo_ftplugin or '',
  'setlocal commentstring< foldexpr< indentexpr<',
}, ' | ')
