# aion-awl-lsp

`aion awl lsp` is the Language Server Protocol adapter for AWL rev-2. It uses
stdio, UTF-16 positions, and full-document synchronization. Parsing,
diagnostics, and formatting come directly from `aion-awl`; the server does not
implement a second language front end.

The server provides:

- parser and checker diagnostics on open and change, cleared on close;
- canonical whole-document formatting;
- document symbols for workflows, types, workers/actions, and steps/substeps;
- checker-resolved definitions for types, fields, actions/children, steps,
  outcomes, signals, inputs, and scoped bindings;
- Markdown hover with checker-computed types, declaration kinds, and attached
  documentation.

Hover and definitions consume `aion_awl::semantic`, a read-only trace of the
normal checker pipeline. The server does not infer types or reproduce checker
scoping rules.

## Helix

Add an AWL language definition and server to `languages.toml`:

```toml
[language-server.aion-awl]
command = "aion"
args = ["awl", "lsp"]

[[language]]
name = "awl"
scope = "source.awl"
file-types = ["awl"]
language-servers = ["aion-awl"]
```

## Neovim

Start the server for an AWL buffer with Neovim's built-in LSP client:

```lua
vim.lsp.start({
  name = "aion-awl",
  cmd = { "aion", "awl", "lsp" },
  root_dir = vim.fs.root(0, { ".git" }) or vim.fn.getcwd(),
})
```
