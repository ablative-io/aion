-module(aion_parent_fixture).
-export([child_round_trip/1, child_then_signal/1, two_children/1]).

%% Spawn one child workflow through the engine NIF bridge, await its
%% terminal result, and return both identifiers so tests can assert the
%% replayed child identity matches recorded history.
child_round_trip(_Input) ->
    {ok, ChildId} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"child-input\"">>, <<"{}">>),
    {ok, ChildResult} = aion_flow_ffi:await_child(ChildId),
    {ChildId, ChildResult}.

%% Same as child_round_trip/1, but gate completion on a "release" signal so
%% tests can crash/restart the engine after the child workflow finished and
%% observe replay before the parent run completes.
child_then_signal(_Input) ->
    {ok, ChildId} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"child-input\"">>, <<"{}">>),
    {ok, ChildResult} = aion_flow_ffi:await_child(ChildId),
    {ok, _Release} = aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>),
    {ChildId, ChildResult}.

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
    {ok, _FirstResult} = aion_flow_ffi:await_child(FirstChild),
    {ok, _SecondResult} = aion_flow_ffi:await_child(SecondChild),
    {ok, _Release} = aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>),
    {FirstChild, SecondChild}.
