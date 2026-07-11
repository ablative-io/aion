if vim.g.loaded_nvim_awl == 1 then
  return
end
vim.g.loaded_nvim_awl = 1

vim.filetype.add({
  extension = {
    awl = 'awl',
  },
})

vim.treesitter.language.register('awl', 'awl')

require('awl').setup()
