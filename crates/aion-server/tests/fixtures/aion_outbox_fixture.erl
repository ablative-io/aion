-module(aion_outbox_fixture).
-export([collect_four/1]).

%% Fixture for the durable-outbox fan-out cutover end-to-end test in
%% tests/outbox_e2e.rs.
%%
%% `collect_four` fans four activities out through the suspending
%% `collect_all` native and returns the encoded result list. With the
%% engine's `outbox.enabled` flag ON, `dispatch_unscheduled` stages these
%% four members through `record_fan_out_dispatch` (atomic events + outbox
%% rows) and spawns NO in-process completion task: the activities never run
%% through the engine's in-process dispatcher. Completions are delivered
%% out-of-band by the test's outbox pump via
%% `RuntimeHandle::deliver_outbox_completion`, which wakes this workflow so
%% its `take_and_record` records each terminal through the store-backed
%% `record_fan_out_completion` dedup primitive.
%%
%% There is no gate protocol and no release signal: the workflow simply
%% parks inside `collect_all` until every ordinal has a recorded terminal,
%% then returns the collected results. The activity names are arbitrary
%% (positional ordinals 0..3 drive routing, not the names).

collect_four(_Input) ->
    Id = <<"collect-four">>,
    Specs = [
        spec(<<"fan:0">>),
        spec(<<"fan:1">>),
        spec(<<"fan:2">>),
        spec(<<"fan:3">>)
    ],
    {ok, Results} = aion_flow_ffi:collect_all(Id, Specs),
    Results.

spec(Name) ->
    <<"{\"name\":\"", Name/binary, "\",\"input\":\"\\\"in\\\"\",\"config\":\"{}\"}">>.
