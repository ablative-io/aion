-module(aion_fixture_workflow).
-export([complete/0, wait/0, activity/1]).

complete() ->
    42.

wait() ->
    receive
        stop -> ok
    end.

activity(_Input) ->
    receive
        stop -> ok
    end.
