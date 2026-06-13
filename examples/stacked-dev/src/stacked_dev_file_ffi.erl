%% File-boundary helper for the `enrich_brief` activity local implementation
%% (and the test suite's fixture emission): read one file as UTF-8 text,
%% write one file (creating parent directories), and remove one directory
%% tree.
%%
%% Like `stacked_dev_cli_ffi`, this module is only ever executed by the
%% `aion/testing` harness — deployed, a Meridian worker serves the activity
%% names and workflow code itself performs no I/O (CN1).
%%
%% Result contracts consumed through `@external` declarations:
%%   read_file/1    {ok, Binary} | {error, Reason-as-binary}
%%   write_file/2   {ok, nil}    | {error, Reason-as-binary}
%%   remove_tree/1  {ok, nil}    | {error, Reason-as-binary}
%%   list_dir/1     {ok, [Binary]} | {error, Reason-as-binary}
%%
%% Every posix failure is rendered loudly into the error binary — a file
%% problem is data for the caller's terminal activity failure, never a crash
%% and never a silent skip.
-module(stacked_dev_file_ffi).

-export([read_file/1, write_file/2, remove_tree/1, list_dir/1]).

read_file(Path) ->
    case file:read_file(binary_to_list(Path)) of
        {ok, Contents} ->
            {ok, Contents};
        {error, Reason} ->
            {error, render(Reason)}
    end.

write_file(Path, Contents) ->
    List = binary_to_list(Path),
    case filelib:ensure_dir(List) of
        ok ->
            case file:write_file(List, Contents) of
                ok -> {ok, nil};
                {error, Reason} -> {error, render(Reason)}
            end;
        {error, Reason} ->
            {error, render(Reason)}
    end.

remove_tree(Path) ->
    case file:del_dir_r(binary_to_list(Path)) of
        ok -> {ok, nil};
        %% An absent tree is the desired end state, not a failure.
        {error, enoent} -> {ok, nil};
        {error, Reason} -> {error, render(Reason)}
    end.

%% List one directory's immediate entry names (cluster directories and ledger
%% files alike) so the assemble_wave resolver can scan for a brief by id. A
%% non-existent or unreadable directory is a loud error binary, never a silent
%% empty list.
list_dir(Path) ->
    case file:list_dir(binary_to_list(Path)) of
        {ok, Names} -> {ok, [list_to_binary(Name) || Name <- Names]};
        {error, Reason} -> {error, render(Reason)}
    end.

render(Reason) ->
    list_to_binary(io_lib:format("~p", [Reason])).
