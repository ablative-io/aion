-module(aion_collect_fixture).
-export([all_two/1, all_fail_fast/1, race_two/1, race_fail/1, all_timeout/1,
         queryable_all/1]).

%% Fixtures for the two-phase collect_* end-to-end tests in
%% tests/concurrency_e2e.rs. Every entry fans activities out through the
%% suspending collect natives behind a hand-rolled query pump (the same raw
%% sentinel protocol as aion_fixture_query), gates completion on a "release"
%% signal so tests can crash/restart the engine with recorded collect
%% terminals in place, and returns the collect's outcome as the workflow
%% result so cross-run history-shape comparisons pin the resolved value.
%%
%% Activity names use the test dispatcher's gate protocol:
%%   <<"gated_ok:K">>   blocks until the test releases gate K, then succeeds
%%   <<"gated_fail:K">> blocks until the test releases gate K, then fails

%% collect_all over two gated activities; returns the encoded result list.
all_two(_Input) ->
    %% Args are precomputed so the fun body is a frameless tail call into
    %% the suspending native: beamr re-executes the stored call instruction
    %% on every wake, so the call site must be re-execution-safe.
    Id = <<"all-two">>,
    Specs = [spec_ok(<<"a">>), spec_ok(<<"b">>)],
    {ok, Results} = pumped(fun() -> aion_flow_ffi:collect_all(Id, Specs) end),
    gate_release(),
    Results.

%% collect_all where one member fails: fail-fast surfaces the failure
%% message; the fixture returns it as JSON so the run completes normally.
all_fail_fast(_Input) ->
    Id = <<"all-fail">>,
    Specs = [spec_ok(<<"a">>), spec_fail(<<"b">>)],
    {error, Reason} = pumped(fun() -> aion_flow_ffi:collect_all(Id, Specs) end),
    gate_release(),
    as_json_string(Reason).

%% collect_race over two gated activities; returns the winner's payload.
race_two(_Input) ->
    Id = <<"race-two">>,
    Specs = [spec_ok(<<"a">>), spec_ok(<<"b">>)],
    {ok, Winner} = pumped(fun() -> aion_flow_ffi:collect_race(Id, Specs) end),
    gate_release(),
    Winner.

%% collect_race where the first settler fails: first-settle semantics make
%% the failure win; the fixture returns its message as JSON.
race_fail(_Input) ->
    Id = <<"race-fail">>,
    Specs = [spec_fail(<<"a">>), spec_ok(<<"b">>)],
    {error, Reason} = pumped(fun() -> aion_flow_ffi:collect_race(Id, Specs) end),
    gate_release(),
    as_json_string(Reason).

%% collect_all under an expiring with_timeout scope: nothing ever releases
%% the gates, the deadline wins, the collect aborts with the canonical
%% scope error and durable cancellations.
all_timeout(_Input) ->
    Id = <<"all-timeout">>,
    Specs = [spec_ok(<<"a">>), spec_ok(<<"b">>)],
    Await = fun() -> aion_flow_ffi:collect_all(Id, Specs) end,
    {error, <<"timeout:deadline expired">>} =
        aion_flow_ffi:with_timeout(<<"300">>, fun() -> pumped(Await) end),
    gate_release(),
    <<"\"timed_out\"">>.

%% Queryable variant of all_two: registers a "state" handler first, so a
%% test can query the workflow while it is parked inside collect_all.
queryable_all(Input) ->
    ok = register_handler(<<"state">>, state_handler()),
    all_two(Input).

%% --- helpers -----------------------------------------------------------------

gate_release() ->
    {ok, _Release} =
        pumped(fun() -> aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>) end),
    ok.

spec_ok(Key) -> spec(<<"gated_ok:", Key/binary>>).
spec_fail(Key) -> spec(<<"gated_fail:", Key/binary>>).

spec(Name) ->
    <<"{\"name\":\"", Name/binary, "\",\"input\":\"\\\"in\\\"\",\"config\":\"{}\"}">>.

%% Wrap a raw engine error message as a JSON string result payload.
as_json_string(Text) ->
    <<"\"", Text/binary, "\"">>.

%% --- handlers ----------------------------------------------------------------

state_handler() ->
    fun(QueryId) ->
        %% A reply that fails (late reply after caller timeout) is
        %% non-fatal by contract; ignore the FFI result.
        _ = aion_flow_ffi:reply_query(
            QueryId,
            <<"{\"answer\":1,\"query_id\":\"", QueryId/binary, "\"}">>
        ),
        ok
    end.

register_handler(Name, Handler) ->
    {ok, _Registered} = aion_flow_ffi:register_query(Name, <<"{}">>),
    erlang:put({aion_query_handler, Name}, Handler),
    ok.

%% --- fixture-local query pump ----------------------------------------------
%%
%% Re-enter the same await after servicing each query sentinel; pass every
%% other outcome through untouched.

pumped(Await) ->
    case Await() of
        {error, <<"aion_query:", Json/binary>>} ->
            ok = service_query(Json),
            pumped(Await);
        Other ->
            Other
    end.

service_query(Json) ->
    QueryId = scan_query_id(Json),
    Name = scan_name(Json),
    case erlang:get({aion_query_handler, Name}) of
        undefined ->
            reply_error(QueryId, <<"no fixture handler for ", Name/binary>>);
        Handler ->
            try
                _ = Handler(QueryId),
                ok
            catch
                error:Reason when is_binary(Reason) ->
                    reply_error(QueryId, <<"handler raised: ", Reason/binary>>);
                _Class:_Reason ->
                    reply_error(QueryId, <<"handler raised">>)
            end
    end.

%% A failed error reply (caller already gone) is non-fatal by contract.
reply_error(QueryId, Message) ->
    _ = aion_flow_ffi:reply_query_error(QueryId, Message),
    ok.

%% --- sentinel JSON field extraction -----------------------------------------
%%
%% The engine emits the sentinel JSON compactly with serde_json. Fixture
%% query names and engine query ids never contain JSON escapes, so scanning
%% for the literal key pattern and copying bytes to the closing quote is
%% exact here (the production SDK pump handles full escaping).

scan_query_id(<<"\"query_id\":\"", Rest/binary>>) -> value_until_quote(Rest, <<>>);
scan_query_id(<<_Byte, Tail/binary>>) -> scan_query_id(Tail);
scan_query_id(<<>>) -> erlang:error(<<"sentinel missing query_id">>).

scan_name(<<"\"name\":\"", Rest/binary>>) -> value_until_quote(Rest, <<>>);
scan_name(<<_Byte, Tail/binary>>) -> scan_name(Tail);
scan_name(<<>>) -> erlang:error(<<"sentinel missing name">>).

value_until_quote(<<$", _Rest/binary>>, Acc) -> Acc;
value_until_quote(<<Byte, Rest/binary>>, Acc) -> value_until_quote(Rest, <<Acc/binary, Byte>>);
value_until_quote(<<>>, _Acc) -> erlang:error(<<"sentinel value unterminated">>).
