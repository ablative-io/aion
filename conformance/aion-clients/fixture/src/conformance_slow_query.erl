-module(conformance_slow_query).
-export([run/1]).

%% Live-server fixture for the query-timeout conformance scenario
%% (conformance/aion-clients/scenarios.json, fixtures.slowQueryWorkflowType).
%%
%% Registers the "slow" query, then parks in a plain Erlang receive with NO
%% query pump — exactly the committed engine fixture's `unpumped` pattern
%% (crates/aion/tests/fixtures/aion_fixture_query.erl) — so a delivered
%% query is never serviced and the caller observes its own deadline as
%% QueryTimeout. The raw receive matches the engine's signal wake marker
%% atom, so the workflow stays parked (Running) for the scenario's lifetime.

run(_Input) ->
    {ok, _Registered} = aion_flow_ffi:register_query(<<"slow">>, <<"{}">>),
    receive
        aion_signal_received -> ok
    end,
    42.
