-module(aion_parent_fixture).
-export([child_round_trip/1, child_then_signal/1, two_children/1, spawn_unloaded/1]).

%% Spawn one child workflow through the engine NIF bridge, await its
%% terminal result, and return both identifiers so tests can assert the
%% replayed child identity matches recorded history.
child_round_trip(_Input) ->
    {ok, ChildId} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"child-input\"">>, <<"{}">>),
    %% await_child returns child success/failure as data with the SDK's
    %% "ok:"/"error:" payload prefixes; {error, _} is an engine fault.
    {ok, <<"ok:", ChildResult/binary>>} = aion_flow_ffi:await_child(ChildId),
    {ChildId, ChildResult}.

%% Same as child_round_trip/1, but gate completion on a "release" signal so
%% tests can crash/restart the engine after the child workflow finished and
%% observe replay before the parent run completes.
child_then_signal(_Input) ->
    {ok, ChildId} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"child-input\"">>, <<"{}">>),
    {ok, <<"ok:", ChildResult/binary>>} = aion_flow_ffi:await_child(ChildId),
    {ok, _Release} = aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>),
    {ChildId, ChildResult}.

%% Spawn a child whose workflow type is never loaded by the test engine.
%% Durable version pinning (#62 D1) resolves the child's package version at
%% record time, so an unloaded child type fails *before* anything is
%% recorded: spawn_child returns {error, _} to workflow code and the parent
%% history carries no ChildWorkflowStarted. Pre-record failures are
%% replay-safe (nothing durable exists to diverge from); F3 still governs
%% every post-record start failure.
spawn_unloaded(_Input) ->
    {error, Reason} = aion_flow_ffi:spawn_child(
        <<"aion_never_loaded_child">>, <<"\"child-input\"">>, <<"{}">>),
    Reason.

%% Spawn two children with a "mid" signal consumed between the spawns, so an
%% asynchronous SignalReceived event lands between the two recorded
%% ChildWorkflowStarted events. Completion is gated on "release" for the
%% same crash/restart choreography as child_then_signal/1.
two_children(_Input) ->
    {ok, FirstChild} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"first-input\"">>, <<"{}">>),
    {ok, _Mid} = aion_flow_ffi:receive_signal(<<"mid">>, <<"{}">>),
    {ok, SecondChild} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"second-input\"">>, <<"{}">>),
    {ok, <<"ok:", _FirstResult/binary>>} = aion_flow_ffi:await_child(FirstChild),
    {ok, <<"ok:", _SecondResult/binary>>} = aion_flow_ffi:await_child(SecondChild),
    {ok, _Release} = aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>),
    {FirstChild, SecondChild}.
