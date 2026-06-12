%% Process-boundary helper for the stacked-dev activity local
%% implementations: run one executable with arguments in a working
%% directory, capturing combined stdout/stderr, the exit status, and the
%% wall-clock duration.
%%
%% This module is only ever executed by the `aion/testing` harness (activity
%% local implementations run in-process there). Deployed, a Meridian worker
%% serves the same activity names and this module is never called — workflow
%% code itself performs no I/O.
%%
%% Result contract consumed by `stacked_dev/cli.gleam`:
%%   {ok, {ExitStatus, Output, DurationMs}}  the process ran to completion
%%   {error, <<"not_found:", Exe>>}          executable absent from PATH
%%   {error, <<"spawn:", Reason>>}           the port could not be opened
%%
%% A non-zero exit status is recorded data, not an error: callers decide what
%% a failed command means (the warm build treats it as a forfeited cache, the
%% check runners treat it as diagnostics).
-module(stacked_dev_cli_ffi).

-export([run_command/3]).

run_command(Executable, Args, Cwd) ->
    case os:find_executable(binary_to_list(Executable)) of
        false ->
            {error, <<"not_found:", Executable/binary>>};
        Path ->
            spawn_command(Path, Args, Cwd)
    end.

spawn_command(Path, Args, Cwd) ->
    Started = erlang:monotonic_time(millisecond),
    try
        erlang:open_port(
            {spawn_executable, Path},
            [
                {args, Args},
                {cd, binary_to_list(Cwd)},
                exit_status,
                stderr_to_stdout,
                binary,
                hide
            ]
        )
    of
        Port -> collect_output(Port, <<>>, Started)
    catch
        error:Reason ->
            {error, list_to_binary(io_lib:format("spawn:~p", [Reason]))}
    end.

collect_output(Port, Acc, Started) ->
    receive
        {Port, {data, Chunk}} ->
            collect_output(Port, <<Acc/binary, Chunk/binary>>, Started);
        {Port, {exit_status, Code}} ->
            Duration = erlang:monotonic_time(millisecond) - Started,
            {ok, {Code, Acc, Duration}}
    end.
