%% Test-only Erlang module occupying the production FFI namespace.
%%
%% The production package ships no src/aion_flow_ffi implementation: the engine
%% registers this namespace at runtime. Gleam tests load this in-process double so
%% the same @external declarations can exercise typed SDK wrappers without a
%% live engine, store, replay loop, or Rust NIF.
-module(aion_flow_ffi).

-export([
    dispatch_activity/3,
    await_activity_result/1,
    now/0,
    random/0,
    random_int/2,
    sleep/1,
    start_timer/2,
    cancel_timer/1,
    with_timeout/2,
    receive_signal/2,
    send_signal/3,
    register_query/2,
    reply_query/2,
    reply_query_error/2,
    dispatch_query/2,
    query_recorded_observations/0,
    spawn_child/3,
    await_child/1,
    collect_all/2,
    collect_race/2,
    collect_map/2,
    testing_reset/0,
    testing_advance/1,
    testing_register_activity_mock/2,
    testing_register_child_mock/2,
    testing_clear_observations/0,
    testing_observations/0,
    testing_query_replies/0,
    testing_enqueue_collect_result/1
]).

-define(DEFAULT_NOW, 1700000000000).

dispatch_activity(Name, Input, Config) ->
    observe(<<"activity:", Name/binary, ":", Input/binary>>),
    Result = case lookup_mock(Name) of
        {ok, Handler} -> Handler(Input);
        error -> legacy_run_activity(Name, Input, Config)
    end,
    CorrelationId = next_activity_correlation(),
    erlang:put({aion_activity_result, self(), CorrelationId}, Result),
    {ok, CorrelationId}.

await_activity_result(CorrelationId) ->
    case erlang:get({aion_activity_result, self(), CorrelationId}) of
        undefined -> {error, <<"terminal:unknown activity correlation">>};
        Result -> Result
    end.

now() ->
    Current = clock_now(),
    observe(<<"now:", (integer_to_binary(Current))/binary>>),
    {ok, integer_to_binary(Current)}.

random() ->
    observe(<<"random:0.25">>),
    {ok, <<"0.25">>}.

random_int(_Min, _Max) ->
    observe(<<"random_int:4">>),
    {ok, <<"4">>}.

sleep(<<"-", _Rest/binary>>) ->
    {error, <<"invalid duration">>};
sleep(Duration) ->
    case parse_int(Duration) of
        {ok, Millis} ->
            Deadline = clock_now() + Millis,
            record_timer(<<"sleep">>, Deadline, fired_status(Deadline)),
            observe(<<"sleep:", Duration/binary, ":", (integer_to_binary(Deadline))/binary>>),
            {ok, <<"fired">>};
        error -> {error, <<"invalid duration">>}
    end.

start_timer(<<"error", _Rest/binary>>, _Duration) ->
    {error, <<"invalid timer">>};
start_timer(TimerId, Duration) ->
    case parse_int(Duration) of
        {ok, Millis} ->
            Deadline = clock_now() + Millis,
            record_timer(TimerId, Deadline, pending_status(Deadline)),
            observe(<<"timer_start:", TimerId/binary, ":", Duration/binary, ":", (integer_to_binary(Deadline))/binary>>),
            {ok, TimerId};
        error -> {error, <<"invalid timer duration">>}
    end.

cancel_timer(<<"cancel-error">>) ->
    {error, <<"timer cancellation failed">>};
cancel_timer(TimerId) ->
    cancel_recorded_timer(TimerId),
    observe(<<"timer_cancel:", TimerId/binary>>),
    {ok, <<"cancelled-or-no-op">>}.

with_timeout(Duration, Operation) ->
    observe(<<"timeout:", Duration/binary>>),
    case Duration of
        <<"0">> -> {error, <<"timeout:deadline expired">>};
        %% Engine fault while arming the scope: NOT a deadline expiry, so the
        %% raw error carries no "timeout:" tag and the SDK must not map it to
        %% TimedOutError.
        <<"-1">> -> {error, <<"durability:scope state missing">>};
        _ -> {ok, Operation()}
    end.

receive_signal(<<"malformed-signal">>, _Config) ->
    observe(<<"signal_receive:malformed-signal">>),
    {ok, <<"{\"order_id\":1,\"cents\":700}">>};
receive_signal(Name, _Config) ->
    observe(<<"signal_receive:", Name/binary>>),
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
    observe(<<"signal_send:", Name/binary, ":", Payload/binary>>),
    {ok, <<"delivered">>}.

%% Mirrors the production register_query/2 NIF: the engine records only the
%% queryable name; the handler fun lives in the process dictionary under
%% {aion_query_handler, Name}, written by aion_flow_query_pump:register/2.
register_query(Name, _Config) ->
    erlang:put({aion_query_registered, self(), Name}, true),
    {ok, <<"registered">>}.

%% Query ids beginning with "dropped-" simulate a caller that stopped
%% waiting (timed-out one-shot sender removed), matching the production
%% NIF's unknown_query_id error string. Successful replies are recorded so
%% pump tests can assert the reply channel was used; replies are NOT
%% observations — queries never touch recorded history.
reply_query(<<"dropped-", _/binary>> = QueryId, _Payload) ->
    record_query_reply(<<"failed:", QueryId/binary>>),
    {error, <<"unknown_query_id:", QueryId/binary>>};
reply_query(QueryId, Payload) ->
    record_query_reply(<<"ok:", QueryId/binary, ":", Payload/binary>>),
    {ok, Payload}.

reply_query_error(<<"dropped-", _/binary>> = QueryId, _Message) ->
    record_query_reply(<<"failed:", QueryId/binary>>),
    {error, <<"unknown_query_id:", QueryId/binary>>};
reply_query_error(QueryId, Message) ->
    record_query_reply(<<"error:", QueryId/binary, ":", Message/binary>>),
    {ok, <<"replied">>}.

dispatch_query(Name, _Config) ->
    case erlang:get({aion_query_registered, self(), Name}) of
        undefined ->
            {error, <<"unknown:", Name/binary>>};
        true ->
            case erlang:get({aion_query_handler, Name}) of
                undefined ->
                    {error,
                        <<"handler_failed:no handler registered for query ", Name/binary>>};
                Handler -> Handler(next_query_id())
            end
    end.

query_recorded_observations() ->
    {ok, integer_to_binary(length(observations()))}.

%% Registered child doubles take precedence over the legacy hardcoded
%% fixtures, mirroring activity mock dispatch: workflow projects register a
%% typed child handler (usually the child module's real `execute` function)
%% through `aion/testing.mock_child`, and the handler runs synchronously at
%% spawn time so the recorded terminal is available to the await.
spawn_child(Name, Input, _Config) ->
    observe(<<"child_spawn:", Name/binary, ":", Input/binary>>),
    ChildId = next_child_id(),
    Result = case lookup_child_mock(Name) of
        {ok, Handler} -> Handler(Input);
        error -> child_result(Name, Input)
    end,
    erlang:put({aion_child_result, self(), ChildId}, Result),
    {ok, ChildId}.

await_child(ChildId) ->
    observe(<<"child_await:", ChildId/binary>>),
    case erlang:get({aion_child_result, self(), ChildId}) of
        undefined -> {error, <<"unknown child">>};
        %% One-shot query sentinel: the first await surfaces a pending query
        %% (the engine's yield-point behavior), and the pump's re-entered
        %% await resolves with the real terminal. Only the `queried-child`
        %% type stores this shape.
        {sentinel_then, Sentinel, Result} ->
            erlang:put({aion_child_result, self(), ChildId}, Result),
            Sentinel;
        Result -> Result
    end.

%% Scripted collect results mirror the engine's collect yield point: a test
%% enqueues `{error, <<"aion_query:", Json/binary>>}` sentinels (and may
%% finish with a scripted resolution) which surface, one per invocation,
%% before the collect's own settlement. The SDK pump must service each
%% sentinel and re-enter the same collect.
collect_all(_CollectionId, Items) ->
    case take_collect_result() of
        {scripted, Result} -> Result;
        none -> collect_all_loop(Items, [])
    end.

collect_race(_CollectionId, Items) ->
    case take_collect_result() of
        {scripted, Result} ->
            Result;
        none ->
            case Items of
                [] -> {error, <<"empty race">>};
                _ ->
                    Winner = earliest_activity(Items),
                    erlang:put({aion_race_cancelled, self()}, length(Items) - 1),
                    activity_result(Winner)
            end
    end.

collect_map(CollectionId, Items) ->
    observe(<<"collect_map:", CollectionId/binary>>),
    collect_all(CollectionId, Items).

testing_reset() ->
    clear_process_state(),
    erlang:put({aion_clock, self()}, ?DEFAULT_NOW),
    erlang:put({aion_observations, self()}, []),
    {ok, pid_key()}.

testing_advance(Duration) ->
    case parse_int(Duration) of
        {ok, Millis} when Millis >= 0 ->
            NewNow = clock_now() + Millis,
            erlang:put({aion_clock, self()}, NewNow),
            resolve_timers(NewNow),
            observe(<<"advance:", Duration/binary, ":", (integer_to_binary(NewNow))/binary>>),
            {ok, integer_to_binary(NewNow)};
        _ -> {error, <<"invalid advance duration">>}
    end.

testing_register_activity_mock(Name, Handler) ->
    Key = {aion_activity_mock, self(), Name},
    erlang:put(Key, Handler),
    {ok, <<"registered">>}.

%% The handler is the type-erased child double registered by
%% `aion/testing.mock_child`: it receives the encoded child input and returns
%% `{ok, <<"ok:", Output/binary>>}` / `{ok, <<"error:", Error/binary>>}` for
%% recorded child terminals, or `{error, Reason}` for an engine-level fault.
testing_register_child_mock(Name, Handler) ->
    Key = {aion_child_mock, self(), Name},
    erlang:put(Key, Handler),
    {ok, <<"registered">>}.

lookup_child_mock(Name) ->
    case erlang:get({aion_child_mock, self(), Name}) of
        undefined -> error;
        Handler -> {ok, Handler}
    end.

testing_enqueue_collect_result(Result) ->
    Key = {aion_collect_script, self()},
    Queue = case erlang:get(Key) of
        undefined -> [];
        Existing -> Existing
    end,
    erlang:put(Key, Queue ++ [Result]),
    {ok, <<"enqueued">>}.

take_collect_result() ->
    Key = {aion_collect_script, self()},
    case erlang:get(Key) of
        undefined -> none;
        [] -> none;
        [Result | Rest] ->
            erlang:put(Key, Rest),
            {scripted, Result}
    end.

testing_clear_observations() ->
    erlang:put({aion_observations, self()}, []),
    {ok, <<"cleared">>}.

testing_observations() ->
    {ok, json_string_array(observations())}.

%% Test-double diagnostic channel for the query pump: every reply attempt is
%% recorded as "ok:Id:Payload", "error:Id:Message", or "failed:Id"
%% (simulated dropped caller). Distinct from observations because query
%% replies never appear in recorded history.
testing_query_replies() ->
    {ok, json_string_array(query_replies())}.

legacy_run_activity(<<"charge-payment">>, Input, _Config) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"{\"id\":\"receipt-", OrderId/binary, "\",\"approved\":true}">>};
legacy_run_activity(<<"slow-charge-payment">>, Input, _Config) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"{\"id\":\"slow-receipt-", OrderId/binary, "\",\"approved\":true}">>};
legacy_run_activity(<<"race-fail-fast">>, _Input, _Config) ->
    {error, <<"terminal:race failed first">>};
legacy_run_activity(<<"fail-retryable">>, _Input, _Config) ->
    {error, <<"retryable:mock retry">>};
legacy_run_activity(<<"malformed-receipt">>, _Input, _Config) ->
    {ok, <<"{\"id\":1,\"approved\":true}">>};
legacy_run_activity(_Name, _Input, _Config) ->
    {error, <<"terminal:unknown activity">>}.

lookup_mock(Name) ->
    case erlang:get({aion_activity_mock, self(), Name}) of
        undefined -> error;
        Handler -> {ok, Handler}
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
    case dispatch_activity(Name, Input, <<"{}">>) of
        {ok, CorrelationId} -> await_activity_result(CorrelationId);
        {error, Reason} -> {error, Reason}
    end.

next_activity_correlation() ->
    Key = {aion_activity_correlation, self()},
    Current = case erlang:get(Key) of undefined -> 0; Value -> Value end,
    erlang:put(Key, Current + 1),
    <<"activity:", (integer_to_binary(Current))/binary>>.

next_query_id() ->
    Key = {aion_query_counter, self()},
    Next = case erlang:get(Key) of undefined -> 1; Existing -> Existing + 1 end,
    erlang:put(Key, Next),
    <<"query-", (integer_to_binary(Next))/binary>>.

query_replies() ->
    case erlang:get({aion_query_replies, self()}) of
        undefined -> [];
        Existing -> Existing
    end.

record_query_reply(Entry) ->
    erlang:put({aion_query_replies, self()}, query_replies() ++ [Entry]).

earliest_activity([First | Rest]) ->
    lists:foldl(
        fun(Spec, Current) ->
            case activity_delay(Spec) < activity_delay(Current) of
                true -> Spec;
                false -> Current
            end
        end,
        First,
        Rest
    ).

activity_delay(Spec) ->
    Name = extract_string(Spec, <<"name">>),
    case Name of
        <<"slow-charge-payment">> -> 20;
        <<"race-fail-fast">> -> 1;
        _ -> 10
    end.

child_result(<<"checkout-child">>, Input) ->
    OrderId = extract_string(Input, <<"order_id">>),
    {ok, <<"ok:{\"id\":\"child-receipt-", OrderId/binary, "\",\"approved\":true}">>};
child_result(<<"declining-child">>, _Input) ->
    {ok, <<"error:\"declined\"">>};
child_result(<<"malformed-child">>, _Input) ->
    {ok, <<"ok:{\"id\":1,\"approved\":true}">>};
child_result(<<"queried-child">>, _Input) ->
    %% First await yields a query sentinel; the re-entered await resolves.
    {sentinel_then,
        {error, <<"aion_query:{\"query_id\":\"q-child\",\"name\":\"child-state\"}">>},
        {ok, <<"ok:\"queried-child-receipt\"">>}};
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

clock_now() ->
    case erlang:get({aion_clock, self()}) of
        undefined -> ?DEFAULT_NOW;
        Existing -> Existing
    end.

record_timer(TimerId, Deadline, Status) ->
    Key = {aion_timers, self()},
    Timers = case erlang:get(Key) of
        undefined -> [];
        Existing -> Existing
    end,
    erlang:put(Key, [{TimerId, Deadline, Status} | Timers]).

cancel_recorded_timer(TimerId) ->
    Key = {aion_timers, self()},
    Timers = case erlang:get(Key) of
        undefined -> [];
        Existing -> Existing
    end,
    erlang:put(Key, [{Id, Deadline, timer_cancel_status(Id, TimerId, Status)} || {Id, Deadline, Status} <- Timers]).

timer_cancel_status(TimerId, TimerId, _Status) -> cancelled;
timer_cancel_status(_Id, _TimerId, Status) -> Status.

resolve_timers(Now) ->
    Key = {aion_timers, self()},
    Timers = case erlang:get(Key) of
        undefined -> [];
        Existing -> Existing
    end,
    erlang:put(Key, [{Id, Deadline, resolve_status(Deadline, Status, Now)} || {Id, Deadline, Status} <- Timers]).

resolve_status(Deadline, pending, Now) when Deadline =< Now -> fired;
resolve_status(_Deadline, Status, _Now) -> Status.

pending_status(Deadline) ->
    case Deadline =< clock_now() of
        true -> fired;
        false -> pending
    end.

fired_status(_Deadline) -> fired.

observations() ->
    case erlang:get({aion_observations, self()}) of
        undefined -> [];
        Existing -> Existing
    end.

observe(Event) ->
    Key = {aion_observations, self()},
    erlang:put(Key, observations() ++ [Event]).

clear_process_state() ->
    Keys = erlang:get_keys(),
    lists:foreach(
        fun(Key) ->
            case is_aion_key(Key) of
                true -> erlang:erase(Key);
                false -> ok
            end
        end,
        Keys
    ).

is_aion_key({aion_signal, Pid, _Name}) -> Pid =:= self();
is_aion_key({aion_query_registered, Pid, _Name}) -> Pid =:= self();
is_aion_key({aion_query_replies, Pid}) -> Pid =:= self();
is_aion_key({aion_query_counter, Pid}) -> Pid =:= self();
%% The production handler key carries no pid: the process dictionary is
%% per-process already (engine contract: {aion_query_handler, NameBinary}).
is_aion_key({aion_query_handler, _Name}) -> true;
is_aion_key({aion_observations, Pid}) -> Pid =:= self();
is_aion_key({aion_child_result, Pid, _ChildId}) -> Pid =:= self();
is_aion_key({aion_child_counter, Pid}) -> Pid =:= self();
is_aion_key({aion_race_cancelled, Pid}) -> Pid =:= self();
is_aion_key({aion_collect_script, Pid}) -> Pid =:= self();
is_aion_key({aion_all_cancelled, Pid}) -> Pid =:= self();
is_aion_key({aion_clock, Pid}) -> Pid =:= self();
is_aion_key({aion_timers, Pid}) -> Pid =:= self();
is_aion_key({aion_activity_mock, Pid, _Name}) -> Pid =:= self();
is_aion_key({aion_child_mock, Pid, _Name}) -> Pid =:= self();
is_aion_key(_Key) -> false.

pid_key() ->
    list_to_binary(erlang:pid_to_list(self())).

parse_int(Bin) when is_binary(Bin) ->
    case string:to_integer(binary_to_list(Bin)) of
        {Value, []} -> {ok, Value};
        _ -> error
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
