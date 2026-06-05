%% Test-only Erlang module occupying the production FFI namespace.
%%
%% AF-001 declares the Gleam @external signatures only; later testing briefs add
%% concrete in-process implementations for these functions. Keeping this module
%% syntactically valid lets `gleam build` compile the package without introducing
%% engine mechanics or Rust/NIF code into aion_flow.
-module(aion_flow_ffi).
