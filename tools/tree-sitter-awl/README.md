# tree-sitter-awl

Tree-sitter grammar and editor queries for the Aion Workflow Language (AWL)
rev-2 surface. This parser is a presentation layer for highlighting and
structural editing; `crates/aion-awl` remains the only parser/checker used for
diagnostics.

## Design

AWL's canonical parser uses indentation tokens, but editor highlighting does
not need to reconstruct semantic block ownership. The rev-2 surface gives each
block a keyword-led opening, so this grammar deliberately recognizes those
structural anchors and lexes the complete surface without an external scanner.
It is newline/indent tolerant, remains useful while a document is being typed,
and never acts as a validity checker. In particular, raw inline JSON Schema is
accepted as presentation syntax while semantic validation stays in
`aion-awl`.

The generated `src/parser.c` is committed. Consumers do not need a tree-sitter
CLI. Maintainers regenerate with tree-sitter CLI **0.26.10**, pinned exactly in
`package.json`:

```sh
cd tools/tree-sitter-awl
npm install
npx tree-sitter generate
```

The checked-in parser was generated equivalently with:

```sh
tree-sitter --version  # tree-sitter 0.26.10
tree-sitter generate
```

## Verification

From this directory:

```sh
tree-sitter test
./test/corpus-sweep.sh
./test/highlight-smoke.sh
```

The corpus sweep discovers every `*/valid/*.awl` fixture, both rev-2 design
examples, and `examples/awl-hello/awl_hello.awl`; it rejects any `ERROR` or
`MISSING` node. The highlight smoke test runs the highlighter over all three
flagship paths and compares `awl_hello` captures to the committed golden.

## Neovim

Install this checkout as an nvim-treesitter local parser (the API name can vary
slightly by nvim-treesitter release):

```lua
local parser_config = require("nvim-treesitter.parsers").get_parser_configs()
parser_config.awl = {
  install_info = {
    url = "/absolute/path/to/aion/tools/tree-sitter-awl",
    files = { "src/parser.c" },
    generate_requires_npm = false,
    requires_generate_from_grammar = false,
  },
  filetype = "awl",
}
vim.filetype.add({ extension = { awl = "awl" } })
```

Copy or link `queries/*.scm` under
`~/.config/nvim/queries/awl/`, then run `:TSInstall awl`.

## Helix

Add the grammar source and language entry to `languages.toml`, replacing the
path with this checkout:

```toml
[[grammar]]
name = "awl"
source = { path = "/absolute/path/to/aion/tools/tree-sitter-awl" }

[[language]]
name = "awl"
scope = "source.awl"
file-types = ["awl"]
comment-token = "//"
roots = []
grammar = "awl"
```

Link or copy `queries/highlights.scm`, `queries/folds.scm`, and
`queries/indents.scm` into Helix's `runtime/queries/awl/` directory, then run
`hx --grammar fetch && hx --grammar build`.

## Publishing: this directory is authoritative; the standalone repo is a mirror

Pair-ruled 2026-07-12. This directory is the AUTHORITATIVE home of the AWL
grammar — it co-evolves with the parser, corpus, and checker in single
commits. The public standalone repo
(`https://github.com/tomWhiting/tree-sitter-awl`, transferring to
`ablative-io` once the org repo can be public) is the **published mirror**
that editor toolchains fetch anonymously (Zed grammar fetch,
nvim-treesitter remote install, web-tree-sitter).

**If you change this grammar, you own the mirror push.** And
**publish-before-pin**: any extension `rev` (editors/zed-awl/extension.toml,
nvim-awl's default `install_info`) may only name a sha VERIFIED PRESENT on
the public mirror — one anonymous `git ls-remote` before the pin lands. A
pin naming an unpushed sha works from an authenticated checkout and dies
from an anonymous fetch. Mechanizing this check into the gates for any diff
touching a grammar rev pin is sanctioned follow-on work.
