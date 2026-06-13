%% Test-only file access for fixture loading: read one file as UTF-8 text.
%%
%% Result contract consumed by `support/fixtures.gleam`:
%%   {ok, Binary}      the file's full contents
%%   {error, Binary}   the posix reason rendered as text (enoent, eacces, ...)
%%
%% Tests never write through this module — fixtures are read-only contracts.
-module(fixtures_ffi).

-export([read_file/1]).

read_file(Path) ->
    case file:read_file(binary_to_list(Path)) of
        {ok, Contents} ->
            {ok, Contents};
        {error, Reason} ->
            {error, list_to_binary(io_lib:format("~p: ~ts", [Reason, Path]))}
    end.
