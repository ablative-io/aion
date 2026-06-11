-module(aion_child_fixture).
-export([complete/1, wait/1]).

%% Terminal child workflow: completes immediately with a known result.
complete(_Input) ->
    42.

%% Long-running child workflow: blocks in receive so tests can observe a
%% live child execution before any terminal outcome is recorded.
wait(_Input) ->
    receive
        stop -> ok
    end.
