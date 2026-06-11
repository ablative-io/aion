-module(aion_fixture_query).
-export([queryable/1, staged/1, unpumped/1, busy/1]).

%% Query end-to-end fixture for tests/engine_query.rs.
%%
%% The engine answers workflow queries at yield points: when a query is
%% pending, each suspending await returns the sentinel
%% `{error, <<"aion_query:", Json/binary>>}` with
%% `Json = {"query_id":"<uuid>","name":"<query name>"}` instead of
%% resolving. This module hand-rolls the pump loop (instead of depending on
%% the aion_flow SDK's `aion_flow_query_pump`) so the tests prove the raw
%% sentinel protocol: re-enter the same await after servicing each query,
%% fetch the handler fun from the process dictionary under the engine
%% contract key `{aion_query_handler, Name}`, and reply through the
%% `aion_flow_ffi` NIFs.

%% Registers three handlers, then parks on a "release" signal behind the
%% pump. Handlers:
%%   <<"state">>   - replies a payload embedding the query id (so tests can
%%                   assert distinct ids for concurrent queries),
%%   <<"boom">>    - raises, proving a handler raise becomes HandlerFailed
%%                   without crashing the workflow process,
%%   <<"records">> - calls the recording `send_signal` NIF, proving the
%%                   servicing guard refuses recording during a query.
queryable(_Input) ->
    ok = register_handler(<<"state">>, state_handler()),
    ok = register_handler(<<"boom">>, boom_handler()),
    ok = register_handler(<<"records">>, records_handler()),
    {ok, _Release} = receive_released(<<"release">>),
    42.

%% Two-gate variant for restart/replay tests: recorded progress (the "step"
%% signal) exists before the crash point, and replay re-registers the
%% handler by re-executing this code from the top.
staged(_Input) ->
    ok = register_handler(<<"state">>, state_handler()),
    {ok, _Step} = receive_released(<<"step">>),
    {ok, _Release} = receive_released(<<"release">>),
    42.

%% Timeout fixture: registers a handler but parks in a plain Erlang receive
%% with NO pump, so a delivered query is never serviced and the caller
%% observes its configured timeout. The raw receive matches the engine's
%% signal wake marker atom; the pumped "finish" await afterwards proves the
%% workflow completes cleanly despite the timed-out query (the pump entry
%% check discards a pending query whose caller stopped waiting).
unpumped(_Input) ->
    ok = register_handler(<<"state">>, state_handler()),
    receive
        aion_signal_received -> ok
    end,
    {ok, _Finish} = receive_released(<<"finish">>),
    42.

%% Active-execution fixture: cycles short pumped sleeps so a query arriving
%% mid-loop is answered at the next sleep yield point, then gates on
%% "release".
busy(_Input) ->
    ok = register_handler(<<"state">>, state_handler()),
    ok = busy_loop(40),
    {ok, _Release} = receive_released(<<"release">>),
    42.

busy_loop(0) ->
    ok;
busy_loop(N) ->
    {ok, _Fired} = pumped(fun() -> aion_flow_ffi:sleep(<<"20">>) end),
    busy_loop(N - 1).

%% --- handlers ------------------------------------------------------------

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

boom_handler() ->
    fun(_QueryId) -> erlang:error(<<"fixture boom">>) end.

records_handler() ->
    fun(QueryId) ->
        %% send_signal records SignalSent; the per-pid servicing guard must
        %% refuse it while a query handler runs. The query id is uuid-shaped
        %% so it would parse as a workflow id if the guard ever let it
        %% through - the {ok, _} arm below would then silently record.
        case aion_flow_ffi:send_signal(QueryId, <<"noop">>, <<"{}">>) of
            {error, Reason} ->
                erlang:error(Reason);
            {ok, _Sent} ->
                _ = aion_flow_ffi:reply_query(QueryId, <<"{\"recorded\":true}">>),
                ok
        end
    end.

register_handler(Name, Handler) ->
    {ok, _Registered} = aion_flow_ffi:register_query(Name, <<"{}">>),
    erlang:put({aion_query_handler, Name}, Handler),
    ok.

%% --- fixture-local query pump --------------------------------------------

receive_released(Name) ->
    pumped(fun() -> aion_flow_ffi:receive_signal(Name, <<"{}">>) end).

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

%% --- sentinel JSON field extraction ---------------------------------------
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
