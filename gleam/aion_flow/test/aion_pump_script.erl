%% Test-only scripted suspending await for query-pump tests.
%%
%% Pump tests drive `aion/internal/pump.run` with a fake await thunk whose
%% successive results are enqueued up front: sentinel results
%% ({error, <<"aion_query:...">>}) simulate the engine surfacing pending
%% queries at a yield point, and the final non-sentinel result simulates the
%% await's own resolution. The queue lives in the test process dictionary so
%% concurrent gleeunit test processes never share state.
-module(aion_pump_script).

-export([reset/0, enqueue/1, take/0]).

reset() ->
    erlang:put({aion_pump_script, self()}, []),
    nil.

enqueue(Result) ->
    Key = {aion_pump_script, self()},
    Queue =
        case erlang:get(Key) of
            undefined -> [];
            Existing -> Existing
        end,
    erlang:put(Key, Queue ++ [Result]),
    nil.

take() ->
    Key = {aion_pump_script, self()},
    case erlang:get(Key) of
        [Next | Rest] ->
            erlang:put(Key, Rest),
            Next;
        _Empty ->
            {error, <<"script_exhausted:pump re-entered an await with no scripted result">>}
    end.
