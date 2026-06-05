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
    send_signal/3,
    register_query/3,
    reply_query/2,
    dispatch_query/2,
    query_recorded_observations/0,
    spawn_child/3,
    await_child/1,
    collect_all/2,
    collect_race/2
]).

run_activity(<<"charge-payment">>, Input, _Config) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"{\"id\":\"receipt-", OrderId/binary, "\",\"approved\":true}">>};
run_activity(<<"slow-charge-payment">>, Input, _Config) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"{\"id\":\"slow-receipt-", OrderId/binary, "\",\"approved\":true}">>};
run_activity(<<"race-fail-fast">>, _Input, _Config) ->
    {error, <<"terminal:race failed first">>};
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

register_query(Name, Handler, _Config) ->
    Key = {aion_query, self(), Name},
    erlang:put(Key, Handler),
    {ok, <<"registered">>}.

reply_query(_QueryId, Payload) ->
    {ok, Payload}.

dispatch_query(Name, _Config) ->
    Key = {aion_query, self(), Name},
    case erlang:get(Key) of
        undefined -> {error, <<"unknown:", Name/binary>>};
        Handler -> Handler(<<"query-1">>)
    end.

query_recorded_observations() ->
    Count = case erlang:get({aion_observations, self()}) of
        undefined -> 0;
        Existing -> Existing
    end,
    {ok, integer_to_binary(Count)}.

spawn_child(Name, Input, _Config) ->
    observe(),
    ChildId = next_child_id(),
    Result = child_result(Name, Input),
    erlang:put({aion_child_result, self(), ChildId}, Result),
    {ok, ChildId}.

await_child(ChildId) ->
    case erlang:get({aion_child_result, self(), ChildId}) of
        undefined -> {error, <<"unknown child">>};
        Result -> Result
    end.

collect_all(_CollectionId, Items) ->
    collect_all_loop(Items, []).

collect_race(_CollectionId, Items) ->
    case Items of
        [] -> {error, <<"empty race">>};
        _ ->
            Results = [activity_result(Spec) || Spec <- Items],
            {Delay, Result} = lists:min([{activity_delay(Spec), Result} || {Spec, Result} <- lists:zip(Items, Results)]),
            erlang:put({aion_race_cancelled, self()}, length(Items) - 1),
            _ = Delay,
            Result
    end.

collect_all_loop([], Acc) ->
    Payloads = lists:reverse(Acc),
    {ok, json_string_array(Payloads)};
collect_all_loop([Spec | Rest], Acc) ->
    case activity_result(Spec) of
        {ok, Payload} -> collect_all_loop(Rest, [Payload | Acc]);
        {error, Reason} ->
            erlang:put({aion_all_cancelled, self()}, length(Rest)),
            {error, Reason}
    end.

activity_result(Spec) ->
    Name = extract_string(Spec, <<"name">>),
    Input = extract_string(Spec, <<"input">>),
    run_activity(Name, Input, <<"{}">>).

activity_delay(Spec) ->
    Name = extract_string(Spec, <<"name">>),
    case Name of
        <<"slow-charge-payment">> -> 20;
        <<"race-fail-fast">> -> 1;
        _ -> 10
    end.

json_string_array(Items) ->
    Escaped = [json_string(Item) || Item <- Items],
    <<"[", (join(Escaped, <<",">>))/binary, "]">>.

json_string(Value) ->
    Escaped = binary:replace(Value, <<"\\">>, <<"\\\\">>, [global]),
    Escaped2 = binary:replace(Escaped, <<"\"">>, <<"\\\"">>, [global]),
    <<"\"", Escaped2/binary, "\"">>.

join([], _Separator) ->
    <<>>;
join([First | Rest], Separator) ->
    lists:foldl(fun(Item, Acc) -> <<Acc/binary, Separator/binary, Item/binary>> end, First, Rest).

child_result(<<"checkout-child">>, Input) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"ok:{\"id\":\"child-receipt-", OrderId/binary, "\",\"approved\":true}">>};
child_result(<<"declining-child">>, _Input) ->
    {ok, <<"error:\"declined\"">>};
child_result(<<"malformed-child">>, _Input) ->
    {ok, <<"ok:{\"id\":1,\"approved\":true}">>};
child_result(_Name, _Input) ->
    {error, <<"unknown child workflow">>}.

next_child_id() ->
    Key = {aion_child_counter, self()},
    Next = case erlang:get(Key) of
        undefined -> 1;
        Existing -> Existing + 1
    end,
    erlang:put(Key, Next),
    integer_to_binary(Next).

observe() ->
    Key = {aion_observations, self()},
    Count = case erlang:get(Key) of
        undefined -> 0;
        Existing -> Existing
    end,
    erlang:put(Key, Count + 1).

extract_string(Json, Field) ->
    Pattern = <<"\"", Field/binary, "\":\"">>,
    case binary:split(Json, Pattern) of
        [_Before, AfterField] -> extract_json_string_value(AfterField, <<>>);
        _ -> <<>>
    end.

extract_json_string_value(<<>>, Acc) ->
    Acc;
extract_json_string_value(<<"\\\"", Rest/binary>>, Acc) ->
    extract_json_string_value(Rest, <<Acc/binary, "\"">>);
extract_json_string_value(<<"\\\\", Rest/binary>>, Acc) ->
    extract_json_string_value(Rest, <<Acc/binary, "\\">>);
extract_json_string_value(<<"\"", _Rest/binary>>, Acc) ->
    Acc;
extract_json_string_value(<<Char, Rest/binary>>, Acc) ->
    extract_json_string_value(Rest, <<Acc/binary, Char>>).
