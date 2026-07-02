%% Test-only helpers for the agent-dev suite.
%%
%% counter_next/1 is a per-process invocation counter keyed by an arbitrary
%% string: the aion/testing harness runs the workflow body and every mock
%% handler in the test's own process, so the process dictionary is a safe,
%% test-scoped seam for handlers whose behaviour must vary by call number
%% (e.g. a gate that fails once, then passes). Each test uses its own key.
-module(agent_dev_test_ffi).
-export([counter_next/1]).

counter_next(Key) ->
    Slot = {agent_dev_counter, Key},
    Next =
        case erlang:get(Slot) of
            undefined -> 1;
            Value -> Value + 1
        end,
    erlang:put(Slot, Next),
    Next.
