# nvim-awl

Neovim support for Aion Workflow Language (`*.awl`) files.

- `*.awl` filetype detection
- Tree-sitter highlighting and folding through Neovim's built-in `vim.treesitter`
- optional Tree-sitter indentation and parser installation through `nvim-treesitter`
- automatic startup of `aion awl lsp` through Neovim's built-in LSP client
- opt-in LSP format-on-save

## Requirements

- Neovim 0.11 or newer
- `aion` on `$PATH` for language-server features
- an AWL Tree-sitter parser, installed using one of the methods below

There are no mandatory Neovim plugin dependencies. `nvim-treesitter` is only a convenience for installing the parser and providing its experimental indentation engine.

## Install

### lazy.nvim

Point lazy.nvim at this standard plugin directory in an Aion checkout:

```lua
{
  dir = '/path/to/aion/editors/nvim-awl',
  ft = 'awl',
  config = function()
    require('awl').setup()
  end,
}
```

Do not use `ft = 'awl'` if this plugin is the only code that detects AWL files: a filetype-lazy plugin cannot detect the filetype that causes itself to load. Either omit `ft`, as below, or add the `vim.filetype.add` rule to your own configuration:

```lua
{
  dir = '/path/to/aion/editors/nvim-awl',
  config = true,
}
```

To use `nvim-treesitter` for parser installation, make it an optional dependency and run `:TSInstall awl` after startup:

```lua
{
  dir = '/path/to/aion/editors/nvim-awl',
  dependencies = { 'nvim-treesitter/nvim-treesitter' },
  config = function()
    require('awl').setup()
  end,
}
```

The current `nvim-treesitter` main branch may require a newer Neovim than 0.11. Pin a release compatible with your Neovim build if necessary.

### packer.nvim

```lua
use {
  '/path/to/aion/editors/nvim-awl',
  config = function()
    require('awl').setup()
  end,
}
```

### Plain runtimepath / custom Neovim build

Either copy this directory into a native package location:

```sh
mkdir -p ~/.local/share/nvim/site/pack/awl/start
cp -R /path/to/aion/editors/nvim-awl \
  ~/.local/share/nvim/site/pack/awl/start/nvim-awl
```

or prepend it from `init.lua`:

```lua
vim.opt.runtimepath:prepend('/path/to/aion/editors/nvim-awl')
```

No plugin manager is required. This is also the recommended layout for hand-rolled Neovim builds: ensure the plugin directory is on `runtimepath` and ensure a parser named `awl` is available under a `parser/` directory on that runtimepath.

## Tree-sitter parser

The plugin ships runtime queries but not a compiled parser.

### Install from the Aion Git repository with nvim-treesitter

The default setup registers this `install_info`:

```lua
require('awl').setup({
  parser = {
    install_info = {
      url = 'https://github.com/ablative-io/aion.git',
      location = 'tools/tree-sitter-awl',
    },
  },
})
```

Then run:

```vim
:TSInstall awl
```

### Install from a local Aion checkout with nvim-treesitter

Current nvim-treesitter accepts `path` for a local parser source. Point it at the repository root and retain the grammar subdirectory:

```lua
require('awl').setup({
  parser = {
    install_info = {
      path = '/path/to/aion',
      location = 'tools/tree-sitter-awl',
    },
  },
})
```

Then run `:TSInstall awl`. Local-path installs use the checkout as-is; revision and branch settings do not apply.

The plugin registers both the current `require('nvim-treesitter.parsers').awl = ...` API and the legacy `get_parser_configs().awl = ...` API. If an unusually lazy setup loads nvim-treesitter after this plugin and does not emit the `TSUpdate` user event, call `require('awl').register_parser()` after loading nvim-treesitter.

### Build the parser directly (no nvim-treesitter)

Neovim's built-in Tree-sitter runtime only needs a shared parser library in `parser/awl.so` on `runtimepath`:

```sh
plugin=/path/to/aion/editors/nvim-awl
mkdir -p "$plugin/parser"
tree-sitter build \
  -o "$plugin/parser/awl.so" \
  /path/to/aion/tools/tree-sitter-awl
```

On platforms that use a different shared-library suffix, use the suffix expected by your Neovim build. Restart Neovim after installing the parser.

The ftplugin calls `vim.treesitter.start()` for AWL buffers and sets the built-in Tree-sitter fold expression. To use folds:

```lua
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'awl',
  callback = function()
    vim.wo.foldmethod = 'expr'
    vim.wo.foldexpr = 'v:lua.vim.treesitter.foldexpr()'
  end,
})
```

If nvim-treesitter exposes its indentation engine, the ftplugin also sets its `indentexpr`; without nvim-treesitter, highlighting, queries, folds, and LSP continue to use only Neovim built-ins.

## LSP

On Neovim 0.11+, the plugin enables the runtime configuration in `lsp/awl.lua` with:

```lua
vim.lsp.enable('awl')
```

It launches the server over stdio as:

```text
aion awl lsp
```

Disable automatic LSP startup if you want to own the configuration:

```lua
require('awl').setup({ lsp = false })
```

You can override the built-in configuration before or after setup with normal Neovim 0.11 APIs:

```lua
vim.lsp.config('awl', {
  cmd = { '/absolute/path/to/aion', 'awl', 'lsp' },
})
vim.lsp.enable('awl')
```

### nvim-lspconfig fallback

For configurations still using the older lspconfig setup style, disable the plugin's built-in activation and register AWL explicitly:

```lua
require('awl').setup({ lsp = false })

local lspconfig = require('lspconfig')
local configs = require('lspconfig.configs')
local util = require('lspconfig.util')

if not configs.awl then
  configs.awl = {
    default_config = {
      cmd = { 'aion', 'awl', 'lsp' },
      filetypes = { 'awl' },
      root_dir = util.root_pattern('.git'),
      single_file_support = true,
    },
  }
end

lspconfig.awl.setup({})
```

## Format on save

Formatting is off by default. Opt in with:

```lua
require('awl').setup({
  format_on_save = true,
  format_timeout_ms = 1000,
})
```

The save hook is buffer-local, is installed only when the `awl` client advertises `textDocument/formatting`, and selects that client by ID when calling `vim.lsp.buf.format()`.

## Configuration defaults

```lua
require('awl').setup({
  lsp = true,
  format_on_save = false,
  format_timeout_ms = 1000,
  parser = {
    install_info = {
      url = 'https://github.com/ablative-io/aion.git',
      location = 'tools/tree-sitter-awl',
    },
  },
})
```

## Verification

From the Aion repository root, with `luac`, `tree-sitter`, and `nvim` on `$PATH`:

```sh
editors/nvim-awl/tests/run.sh
```

The script checks Lua syntax, checks every query against the AWL grammar and example file, builds an isolated parser, and runs a headless Neovim smoke test.
