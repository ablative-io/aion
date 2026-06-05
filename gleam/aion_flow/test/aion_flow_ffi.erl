%% Test-only Erlang module occupying the production FFI namespace.
%%
%% The production package ships no src/aion_flow_ffi implementation: the engine
%% registers this namespace at runtime. Gleam tests load this in-process double so
%% the same @external declarations can exercise typed SDK wrappers without a
%% live engine, store, replay loop, or Rust NIF.
-module(aion_flow_ffi).

-export([run_activity/3, now/0, random/0, random_int/2]).

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

extract_string(Json, Field) ->
    Pattern = <<"\"", Field/binary, "\":\"">>,
    case binary:split(Json, Pattern) of
        [_Before, AfterField] ->
            [Value | _Rest] = binary:split(AfterField, <<"\"">>),
            Value;
        _ ->
            <<>>
    end.
