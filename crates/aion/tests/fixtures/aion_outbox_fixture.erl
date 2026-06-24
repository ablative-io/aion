-module(aion_outbox_fixture).
-export([collect_four/1, collect_race_four/1, collect_map_four/1]).

%% Fixtures for the durable-outbox fan-out cutover end-to-end tests in
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
%% parks inside the collect native until enough ordinals have a recorded
%% terminal to settle, then returns the collected outcome. The activity
%% names are arbitrary (positional ordinals 0..3 drive routing, not names).
%%
%% `collect_race_four` and `collect_map_four` are the same four-member
%% fan-out through the OTHER two collect natives — they share the exact
%% shape-agnostic outbox dispatch (`dispatch_unscheduled` →
%% `record_fan_out_dispatch`) and completion (`take_and_record` →
%% `record_fan_out_completion`) path, exercising the distinct `settle_race`
%% (winner/loser) and `collect_map` NIF entrypoints end-to-end under the flag.

collect_four(_Input) ->
    Id = <<"collect-four">>,
    {ok, Results} = aion_flow_ffi:collect_all(Id, four_specs()),
    Results.

%% Four-member `collect_race` through the outbox: the first delivered
%% completion settles the batch (the winner) and the three unresolved
%% siblings are cancelled by `settle_race`. Returns the winner's payload.
collect_race_four(_Input) ->
    Id = <<"collect-race-four">>,
    {ok, Winner} = aion_flow_ffi:collect_race(Id, four_specs()),
    Winner.

%% Four-member `collect_map` through the outbox: same fan-out semantics as
%% `collect_all` (every member must complete), driven through the distinct
%% `collect_map` native. Returns the encoded result list in input order.
collect_map_four(_Input) ->
    Id = <<"collect-map-four">>,
    {ok, Results} = aion_flow_ffi:collect_map(Id, four_specs()),
    Results.

four_specs() ->
    [
        spec(<<"fan:0">>),
        spec(<<"fan:1">>),
        spec(<<"fan:2">>),
        spec(<<"fan:3">>)
    ].

spec(Name) ->
    <<"{\"name\":\"", Name/binary, "\",\"input\":\"\\\"in\\\"\",\"config\":\"{}\"}">>.
