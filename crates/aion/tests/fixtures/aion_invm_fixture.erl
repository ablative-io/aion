-module(aion_invm_fixture).
-export([
    run_once_gated/1,
    fail_retryable/1,
    crash/1,
    hang/1,
    hang_with_timeout/1,
    bad_thunk/1,
    remote_tier_defense/1
]).

%% Fixtures for the in-VM activity dispatch end-to-end tests in
%% tests/invm_activity_e2e.rs.
%%
%% Every entry dispatches through the arity-4 in-VM wire
%% (aion_flow_ffi:dispatch_activity_in_vm/4): the fourth argument is the
%% runner thunk the engine spawns as a LINKED child process of this workflow
%% process. The thunks close over `Input` (the raw JSON input binary), which
%% doubles as the per-test side-effect counter key for the host-registered
%% `invm_test_host:bump/1` NIF — the runs-once/replay proofs count the
%% runner's real executions per workflow, immune to test parallelism.

%% Dispatch one in-VM activity whose runner bumps the per-key counter and
%% returns the new count as its JSON payload, then gate on a "release" signal
%% so tests can crash/restart the engine with the activity terminal recorded
%% but the run still live (replay must resolve the recording WITHOUT
%% re-running the runner: the counter stays at 1).
run_once_gated(Input) ->
    {ok, Corr} = aion_flow_ffi:dispatch_activity_in_vm(
        <<"invm_work">>, Input, config(), fun() ->
            Count = invm_test_host:bump(Input),
            {ok, integer_to_binary(Count)}
        end),
    {ok, Payload} = aion_flow_ffi:await_activity_result(Corr),
    {ok, _Release} = aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>),
    Payload.

%% The runner returns a typed retryable error: the prefixed reason crosses
%% the child boundary verbatim and the workflow observes it as data.
fail_retryable(Input) ->
    {ok, Corr} = aion_flow_ffi:dispatch_activity_in_vm(
        <<"invm_fail">>, Input, config(), fun() ->
            _ = invm_test_host:bump(Input),
            {error, <<"retryable:boom">>}
        end),
    {error, Reason} = aion_flow_ffi:await_activity_result(Corr),
    <<"\"error:", Reason/binary, "\"">>.

%% The runner crashes (badmatch): the linked child dies abnormally, the
%% watcher synthesizes the terminal reason, and — because workflow processes
%% trap exits — THIS process survives to observe the failure as data.
crash(Input) ->
    {ok, Corr} = aion_flow_ffi:dispatch_activity_in_vm(
        <<"invm_crash">>, Input, config(), fun() ->
            _ = invm_test_host:bump(Input),
            ok = crash_value(),
            {ok, <<"\"unreachable\"">>}
        end),
    {error, Reason} = aion_flow_ffi:await_activity_result(Corr),
    <<"\"error:", Reason/binary, "\"">>.

%% The runner parks forever: used to observe a live in-VM child (cancel
%% propagation through the link) while the workflow is parked in the await.
hang(Input) ->
    {ok, Corr} = aion_flow_ffi:dispatch_activity_in_vm(
        <<"invm_hang">>, Input, config(), fun() ->
            _ = invm_test_host:bump(Input),
            receive never -> {ok, <<"\"unreachable\"">>} end
        end),
    {ok, Payload} = aion_flow_ffi:await_activity_result(Corr),
    Payload.

%% A hanging runner under an expiring with_timeout scope: the deadline wins,
%% the await aborts with the canonical durable timeout failure, and the
%% orphaned child runs until this process exits (accepted semantics, same as
%% a remote worker still executing past its timeout).
hang_with_timeout(Input) ->
    Await = fun() ->
        {ok, Corr} = aion_flow_ffi:dispatch_activity_in_vm(
            <<"invm_hang">>, Input, config(), fun() ->
                _ = invm_test_host:bump(Input),
                receive never -> {ok, <<"\"unreachable\"">>} end
            end),
        aion_flow_ffi:await_activity_result(Corr)
    end,
    {error, <<"timeout:deadline expired">>} =
        aion_flow_ffi:with_timeout(<<"300">>, Await),
    {ok, _Release} = aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>),
    <<"\"timed_out\"">>.

%% Defense: a non-closure fourth argument is refused at decode time — the
%% dispatch returns a workflow-visible error and NOTHING is recorded.
bad_thunk(_Input) ->
    {error, Reason} = aion_flow_ffi:dispatch_activity_in_vm(
        <<"invm_bad">>, <<"\"in\"">>, config(), not_a_fun),
    <<"\"error:", Reason/binary, "\"">>.

%% Defense: tier "in_vm" arriving on the arity-3 REMOTE wire is refused
%% before anything is recorded.
remote_tier_defense(_Input) ->
    {error, Reason} = aion_flow_ffi:dispatch_activity(
        <<"invm_smuggled">>, <<"\"in\"">>, config()),
    <<"\"error:", Reason/binary, "\"">>.

%% The SDK-shaped dispatch config the in-VM wire carries (activity_config
%% with tier "in_vm").
config() ->
    <<"{\"retry\":null,\"timeout_ms\":null,\"heartbeat_ms\":null,\"labels\":{},"
      "\"task_queue\":null,\"workflow_task_queue\":null,\"node\":null,"
      "\"tier\":\"in_vm\"}">>.

%% Opaque to the compiler so the deliberate badmatch in crash/1 does not trip
%% erlc -Werror's no-clause-will-match analysis.
crash_value() ->
    boom.
