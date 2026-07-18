-module(aion_fixture_workflow).
-export([
    complete/0,
    complete/1,
    wait/0,
    wait/1,
    activity/1,
    spawn_children/0,
    overflow_children/0,
    parked_children/0,
    fun_spawn_children/0,
    overflow_fun_spawn_children/0,
    parked_fun_spawn_children/0
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

fun_spawn_children() ->
    fun_spawn_children(32, fun complete/0).

overflow_fun_spawn_children() ->
    fun_spawn_children(550, fun complete/0).

parked_fun_spawn_children() ->
    fun_spawn_children(32, fun wait/0).

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

fun_spawn_children(0, _Child) ->
    ok;
fun_spawn_children(Count, Child) ->
    _ = erlang:spawn(Child),
    _ = erlang:spawn_link(Child),
    fun_spawn_children(Count - 1, Child).
