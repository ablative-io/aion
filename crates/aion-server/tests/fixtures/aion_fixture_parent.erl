%% Parent workflow fixture for aion-server namespace integration tests.
%%
%% Spawns one child workflow through the engine's real child NIF path
%% (`aion_flow_ffi:spawn_child/3`), awaits its result, and completes. The
%% child type is the sibling `aion_fixture_workflow` package loaded with its
%% `complete` entry, so the child finishes immediately with the known result.
%%
%% Any spawn or await failure crashes the process via badmatch, which the
%% engine records as a workflow failure — tests asserting on the parent's
%% successful result therefore fail loudly if the child path breaks.
-module(aion_fixture_parent).
-export([orchestrate/1]).

orchestrate(_Input) ->
    {ok, ChildId} = aion_flow_ffi:spawn_child(
        <<"aion_fixture_workflow">>,
        <<"{\"fixture\":\"child-input\"}">>,
        <<"{}">>
    ),
    {ok, _Result} = aion_flow_ffi:await_child(ChildId),
    42.
