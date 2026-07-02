%% Test-only helpers for the assistant suite.
%%
%% counter_next/1 is a per-process invocation counter keyed by an arbitrary
%% string: the aion/testing harness runs the workflow body and every mock
%% handler in the test's own process, so the process dictionary is a safe,
%% test-scoped seam for handlers whose behaviour must vary by call number
%% (e.g. counting how many assistant rounds actually dispatched). Each test
%% uses its own key.
-module(assistant_test_ffi).
-export([counter_next/1]).

counter_next(Key) ->
    Slot = {assistant_counter, Key},
    Next =
        case erlang:get(Slot) of
            undefined -> 1;
            Value -> Value + 1
        end,
    erlang:put(Slot, Next),
    Next.
