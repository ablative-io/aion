-module(aion_fixture_workflow).
-export([complete/0, complete/1, wait/0, wait/1, activity/1]).

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
