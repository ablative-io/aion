-module(aion_parent_query_fixture).
-export([await_gated/1, queryable_await/1]).

%% Parent fixtures for the two-phase await_child end-to-end tests in
%% tests/child_await_e2e.rs. Both entries spawn one aion_child_fixture
%% child, park in the suspending await_child native behind a hand-rolled
%% query pump (the same raw sentinel protocol as aion_fixture_query), gate
%% completion on a "release" signal, and return only the child's result so
%% cross-run history-shape comparisons never embed child identifiers.

%% Plain pumped parent: no query handler registered. The control/crash
%% determinism tests run this entry with and without a restart.
await_gated(_Input) ->
    {ok, ChildId} = aion_flow_ffi:spawn_child(
        <<"aion_child_fixture">>, <<"\"child-input\"">>, <<"{}">>),
    {ok, <<"ok:", ChildResult/binary>>} =
        pumped(fun() -> aion_flow_ffi:await_child(ChildId) end),
    {ok, _Release} =
        pumped(fun() -> aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>) end),
    ChildResult.

%% Queryable parent: registers a "state" handler before spawning, so a test
%% can query the workflow while it is parked inside await_child. The
%% handler replies a payload embedding the query id.
queryable_await(Input) ->
    ok = register_handler(<<"state">>, state_handler()),
    await_gated(Input).

%% --- handlers --------------------------------------------------------------

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
