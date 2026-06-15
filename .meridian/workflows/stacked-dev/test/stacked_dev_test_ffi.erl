%% Test-only filesystem/environment helpers for the fake-CLI shim harness.
%%
%% The hermetic test suite builds a per-test shim directory of stub scripts
%% (meridian / norn / cargo), points PATH at it, and reads back the argv
%% recordings the shims append. These helpers own the raw file and
%% environment calls so the Gleam test support module stays typed.
-module(stacked_dev_test_ffi).

-export([
    make_shim_root/0,
    write_executable/2,
    put_env/2,
    read_log/1
]).

%% Create a unique shim root containing an empty `workspace` directory (the
%% directory the provision shim hands back as the workspace path).
%%
%% `erlang:unique_integer/1` restarts with every VM, so it alone would let a
%% test run reuse a directory — and its appended argv logs — left under the
%% tmp dir by a previous run. The OS pid disambiguates across runs, and any
%% surviving collision (pid reuse) is deleted before use so every test
%% starts from an empty recording.
make_shim_root() ->
    Unique = os:getpid() ++ "-" ++ integer_to_list(erlang:unique_integer([positive])),
    Root = filename:join(base_tmp_dir(), "aion-stacked-dev-" ++ Unique),
    Workspace = filename:join(Root, "workspace"),
    case clear_stale_root(Root) of
        ok ->
            case filelib:ensure_path(Workspace) of
                ok -> {ok, list_to_binary(Root)};
                {error, Reason} ->
                    {error, format_error("create shim root", Reason)}
            end;
        {error, Reason} ->
            {error, format_error("clear stale shim root", Reason)}
    end.

clear_stale_root(Root) ->
    case filelib:is_dir(Root) of
        true -> file:del_dir_r(Root);
        false -> ok
    end.

%% Write a shim script and mark it executable.
write_executable(Path, Contents) ->
    PathString = binary_to_list(Path),
    case file:write_file(PathString, Contents) of
        ok ->
            case file:change_mode(PathString, 8#755) of
                ok -> {ok, <<"written">>};
                {error, Reason} -> {error, format_error("chmod shim", Reason)}
            end;
        {error, Reason} ->
            {error, format_error("write shim", Reason)}
    end.

%% Set one environment variable for the whole VM (the suite repoints PATH at
%% each test's own shim directory before running the pipeline).
put_env(Name, Value) ->
    true = os:putenv(binary_to_list(Name), binary_to_list(Value)),
    {ok, <<"set">>}.

%% Read a shim's argv recording. A missing file means the shim was never
%% invoked, which is itself an assertable outcome — so it reads as empty.
read_log(Path) ->
    case file:read_file(binary_to_list(Path)) of
        {ok, Contents} -> {ok, Contents};
        {error, enoent} -> {ok, <<>>};
        {error, Reason} -> {error, format_error("read shim log", Reason)}
    end.

base_tmp_dir() ->
    case os:getenv("TMPDIR") of
        false -> "/tmp";
        "" -> "/tmp";
        Dir -> Dir
    end.

format_error(Context, Reason) ->
    list_to_binary(io_lib:format("~s: ~p", [Context, Reason])).
