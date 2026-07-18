-module(aion_fixture_workflow).
-export([
    complete/0,
    complete/1,
    wait/0,
    wait/1,
    activity/1,
    spawn_children/0,
    overflow_children/0,
    parked_children/0
]).

complete() ->
    42.

complete(_Input) ->
    42.

wait() ->
    receive
        stop -> ok
    end.

wait(_Input) ->
    receive
        stop -> ok
    end.

activity(_Input) ->
    receive
        stop -> ok
    end.

spawn_children() ->
    spawn_children(64).

overflow_children() ->
    spawn_children(1100).

parked_children() ->
    parked_children(64).

spawn_children(0) ->
    ok;
spawn_children(Count) ->
    _ = erlang:spawn(?MODULE, complete, []),
    spawn_children(Count - 1).

parked_children(0) ->
    ok;
parked_children(Count) ->
    _ = erlang:spawn(?MODULE, wait, []),
    parked_children(Count - 1).
