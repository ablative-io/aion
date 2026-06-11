-module(aion_child_fixture).
-export([complete/1, wait/1, gated/1, can_once/1]).

%% Terminal child workflow: completes immediately with a known result.
complete(_Input) ->
    42.

%% Long-running child workflow: blocks in receive so tests can observe a
%% live child execution before any terminal outcome is recorded.
wait(_Input) ->
    receive
        stop -> ok
    end.

%% Signal-gated child workflow: parks in the suspending receive_signal
%% native until a "child_go" signal arrives, then completes with the known
%% result. Used to hold a parent parked inside await_child for as long as a
%% test needs (queries, crash/restart choreography).
gated(_Input) ->
    {ok, _Go} = aion_flow_ffi:receive_signal(<<"child_go">>, <<"{}">>),
    42.

%% Continue-as-new child workflow: the first run rotates once via the
%% engine's continue_as_new NIF (which records the terminal and cancels this
%% process), and the replacement run completes with the known result. The
%% awaiting parent must observe only the final run's result under the same
%% stable child workflow id.
can_once(<<"\"second\"">>) ->
    42;
can_once(_Input) ->
    {ok, _Continued} = aion_flow_ffi:continue_as_new(<<"\"second\"">>),
    %% The engine cancels this process after recording the rotation; park
    %% until that kill lands so no result is ever returned from run one.
    receive
        can_fixture_never_resumes -> ok
    end.
