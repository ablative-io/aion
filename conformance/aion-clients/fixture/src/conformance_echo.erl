-module(conformance_echo).
-export([run/1]).

%% Live-server fixture for the shared client conformance scenarios
%% (conformance/aion-clients/scenarios.json, fixtures.workflowType).
%%
%% Behaviour pinned by the scenarios:
%%   - starts with a compact JSON object input (e.g.
%%     {"message":"hello","counter":1}) and keeps running until cancelled
%%     (the engine records the terminal WorkflowCancelled event and stops the
%%     process — cancellation needs no fixture code);
%%   - accepts the "record" signal repeatedly, remembering the latest
%%     payload's "value" field;
%%   - answers the "state" query with the start input's fields plus
%%     "lastSignal" (null until the first record signal);
%%   - lifecycle/signal events are recorded by the engine automatically.
%%
%% The module hand-rolls the engine's raw query-pump sentinel protocol the
%% same way the committed engine fixture (crates/aion/tests/fixtures/
%% aion_fixture_query.erl) does: when a query is pending, a suspending await
%% returns `{error, <<"aion_query:", Json/binary>>}`; the pump services the
%% query through the `aion_flow_ffi` NIFs and re-enters the same await.
%%
%% All binary handling uses literal-prefix matches and accumulator rebuilds
%% only — the byte-level idioms the committed fixtures prove against the
%% beamr VM (no dynamic-size binary segments).

run(Input) ->
    erlang:put(conformance_echo_state, {input_inner(Input), null}),
    ok = register_handler(<<"state">>, fun state_handler/1),
    signal_loop().

%% Park on the "record" signal forever; every delivery updates the
%% remembered lastSignal value. Replay re-executes this loop from the top and
%% resolves each receive from recorded history, so the process-dictionary
%% state is rebuilt deterministically.
signal_loop() ->
    case pumped(fun() -> aion_flow_ffi:receive_signal(<<"record">>, <<"{}">>) end) of
        {ok, Payload} ->
            {Inner, _Previous} = erlang:get(conformance_echo_state),
            erlang:put(conformance_echo_state, {Inner, scan_value(Payload)}),
            signal_loop();
        {error, Reason} ->
            erlang:error(Reason)
    end.

%% --- query handler ---------------------------------------------------------

state_handler(QueryId) ->
    {Inner, LastSignal} = erlang:get(conformance_echo_state),
    %% A reply that fails (late reply after caller timeout) is non-fatal by
    %% contract; ignore the FFI result.
    _ = aion_flow_ffi:reply_query(QueryId, state_json(Inner, LastSignal)),
    ok.

%% Merge the start input object's inner fields with the lastSignal field.
state_json(Inner, LastSignal) ->
    LastSignalJson =
        case LastSignal of
            null -> <<"null">>;
            Value -> <<"\"", Value/binary, "\"">>
        end,
    case Inner of
        <<>> -> <<"{\"lastSignal\":", LastSignalJson/binary, "}">>;
        _ -> <<"{", Inner/binary, ",\"lastSignal\":", LastSignalJson/binary, "}">>
    end.

%% --- input normalization ----------------------------------------------------

%% Returns the inner fields of the compact JSON object input (the bytes
%% between the outer braces). The conformance harness payload encoders all
%% emit compact JSON, so the outer braces are the first and last bytes.
input_inner(Input) when is_binary(Input) ->
    inner_object(Input);
input_inner(Input) when is_list(Input) ->
    inner_object(list_to_binary(Input));
input_inner(_Input) ->
    erlang:error(<<"conformance_echo input payload was not a JSON string">>).

inner_object(<<"{", Rest/binary>>) ->
    take_until_closing_brace(Rest, <<>>);
inner_object(_Json) ->
    erlang:error(<<"conformance_echo input is not a compact JSON object">>).

%% Accumulate every byte up to the FINAL closing brace (the last byte of the
%% document); interior braces of nested objects pass through untouched.
take_until_closing_brace(<<$}>>, Acc) ->
    Acc;
take_until_closing_brace(<<Byte, Rest/binary>>, Acc) ->
    take_until_closing_brace(Rest, <<Acc/binary, Byte>>);
take_until_closing_brace(<<>>, _Acc) ->
    erlang:error(<<"conformance_echo input object is unterminated">>).

%% --- raw sentinel query pump (engine contract) -------------------------------

register_handler(Name, Handler) ->
    {ok, _Registered} = aion_flow_ffi:register_query(Name, <<"{}">>),
    erlang:put({aion_query_handler, Name}, Handler),
    ok.

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

%% --- compact-JSON field scanning ---------------------------------------------
%%
%% The engine emits sentinel JSON compactly with serde_json, and the
%% conformance signal payloads carry plain unescaped string values, so
%% scanning for the literal key pattern and copying bytes to the closing
%% quote is exact here (the production SDK pump handles full escaping).

scan_query_id(<<"\"query_id\":\"", Rest/binary>>) -> value_until_quote(Rest, <<>>);
scan_query_id(<<_Byte, Tail/binary>>) -> scan_query_id(Tail);
scan_query_id(<<>>) -> erlang:error(<<"sentinel missing query_id">>).

scan_name(<<"\"name\":\"", Rest/binary>>) -> value_until_quote(Rest, <<>>);
scan_name(<<_Byte, Tail/binary>>) -> scan_name(Tail);
scan_name(<<>>) -> erlang:error(<<"sentinel missing name">>).

%% The latest record-signal payload's "value" string, or null when absent.
scan_value(<<"\"value\":\"", Rest/binary>>) -> value_until_quote(Rest, <<>>);
scan_value(<<_Byte, Tail/binary>>) -> scan_value(Tail);
scan_value(<<>>) -> null.

value_until_quote(<<$", _Rest/binary>>, Acc) -> Acc;
value_until_quote(<<Byte, Rest/binary>>, Acc) -> value_until_quote(Rest, <<Acc/binary, Byte>>);
value_until_quote(<<>>, _Acc) -> erlang:error(<<"scanned value unterminated">>).
