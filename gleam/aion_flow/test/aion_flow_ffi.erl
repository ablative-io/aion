%% Test-only Erlang module occupying the production FFI namespace.
%%
%% The production package ships no src/aion_flow_ffi implementation: the engine
%% registers this namespace at runtime. Gleam tests load this in-process double so
%% the same @external declarations can exercise typed SDK wrappers without a
%% live engine, store, replay loop, or Rust NIF.
-module(aion_flow_ffi).

-export([
    run_activity/3,
    now/0,
    random/0,
    random_int/2,
    sleep/1,
    start_timer/2,
    cancel_timer/1,
    with_timeout/2,
    receive_signal/2,
    send_signal/3
]).

run_activity(<<"charge-payment">>, Input, _Config) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"{\"id\":\"receipt-", OrderId/binary, "\",\"approved\":true}">>};
run_activity(<<"fail-retryable">>, _Input, _Config) ->
    {error, <<"retryable:mock retry">>};
run_activity(<<"malformed-receipt">>, _Input, _Config) ->
    {ok, <<"{\"id\":1,\"approved\":true}">>};
run_activity(_Name, _Input, _Config) ->
    {error, <<"terminal:unknown activity">>}.

now() ->
    {ok, <<"1700000000000">>}.

random() ->
    {ok, <<"0.25">>}.

random_int(_Min, _Max) ->
    {ok, <<"4">>}.

sleep(<<"-", _Rest/binary>>) ->
    {error, <<"invalid duration">>};
sleep(_Duration) ->
    {ok, <<"fired">>}.

start_timer(<<"error", _Rest/binary>>, _Duration) ->
    {error, <<"invalid timer">>};
start_timer(TimerId, _Duration) ->
    {ok, TimerId}.

cancel_timer(<<"cancel-error">>) ->
    {error, <<"timer cancellation failed">>};
cancel_timer(_TimerId) ->
    {ok, <<"cancelled-or-no-op">>}.

with_timeout(Duration, Operation) ->
    case Duration of
        <<"0">> -> {error, <<"timeout:deadline expired">>};
        _ -> {ok, Operation()}
    end.

receive_signal(<<"malformed-signal">>, _Config) ->
    {ok, <<"{\"order_id\":1,\"cents\":700}">>};
receive_signal(Name, _Config) ->
    Key = {aion_signal, self(), Name},
    case erlang:get(Key) of
        undefined -> {error, <<"unknown:", Name/binary>>};
        [] -> {error, <<"unknown:", Name/binary>>};
        [Payload | Rest] ->
            erlang:put(Key, Rest),
            {ok, Payload}
    end.

send_signal(_WorkflowId, Name, Payload) ->
    Key = {aion_signal, self(), Name},
    Queue = case erlang:get(Key) of
        undefined -> [];
        Existing -> Existing
    end,
    erlang:put(Key, Queue ++ [Payload]),
    {ok, <<"delivered">>}.

extract_string(Json, Field) ->
    Pattern = <<"\"", Field/binary, "\":\"">>,
    case binary:split(Json, Pattern) of
        [_Before, AfterField] ->
            [Value | _Rest] = binary:split(AfterField, <<"\"">>),
            Value;
        _ ->
            <<>>
    end.
