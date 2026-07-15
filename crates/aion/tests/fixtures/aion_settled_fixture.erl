-module(aion_settled_fixture).
-export([settled_three/1]).

%% The settled fan-out wire shape (aion_flow's workflow.map_settled /
%% all_settled, hand-rolled at the FFI boundary for tests/settled_fanout_e2e.rs):
%% dispatch every member through the SINGLE-dispatch wire in item order, then
%% await each correlation id in item order behind the query pump. No
%% fail-fast, no sibling cancellation: each member settles independently and
%% a terminal failure arrives as that slot's value. Completion gates on a
%% "release" signal so tests can crash/restart the engine with the fan-out's
%% terminals recorded but the run still live.
%%
%% Activity names use the test dispatcher's gate protocol:
%%   <<"gated_ok:K">>   blocks until the test releases gate K, then succeeds
%%   <<"gated_fail:K">> blocks until the test releases gate K, then fails
settled_three(_Input) ->
    %% Dispatch ALL members before awaiting ANY (the settled contract).
    {ok, IdA} = aion_flow_ffi:dispatch_activity(<<"gated_ok:a">>, <<"\"in\"">>, <<"{}">>),
    {ok, IdB} = aion_flow_ffi:dispatch_activity(<<"gated_fail:b">>, <<"\"in\"">>, <<"{}">>),
    {ok, IdC} = aion_flow_ffi:dispatch_activity(<<"gated_ok:c">>, <<"\"in\"">>, <<"{}">>),
    %% Await each id in ITEM order; args precomputed so every await fun body
    %% is a frameless re-execution-safe call into the suspending native.
    SlotA = settle(IdA),
    SlotB = settle(IdB),
    SlotC = settle(IdC),
    gate_release(),
    as_json_string(<<SlotA/binary, "|", SlotB/binary, "|", SlotC/binary>>).

%% One member's settled slot: success payload or captured failure, as data.
settle(Id) ->
    case pumped(fun() -> aion_flow_ffi:await_activity_result(Id) end) of
        {ok, Payload} -> <<"ok=", Payload/binary>>;
        {error, Reason} -> <<"err=", Reason/binary>>
    end.

%% Wrap the joined slots as a JSON string result payload.
as_json_string(Text) ->
    <<"\"", Text/binary, "\"">>.

gate_release() ->
    {ok, _Release} =
        pumped(fun() -> aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>) end),
    ok.

%% --- fixture-local query pump ------------------------------------------------
%%
%% Re-enter the same await after refusing each query sentinel (this fixture
%% registers no handlers); pass every other outcome through untouched.

pumped(Await) ->
    case Await() of
        {error, <<"aion_query:", Json/binary>>} ->
            ok = deny_query(Json),
            pumped(Await);
        Other ->
            Other
    end.

%% A failed error reply (caller already gone) is non-fatal by contract.
deny_query(Json) ->
    QueryId = scan_query_id(Json),
    _ = aion_flow_ffi:reply_query_error(QueryId, <<"no fixture handler">>),
    ok.

scan_query_id(<<"\"query_id\":\"", Rest/binary>>) -> value_until_quote(Rest, <<>>);
scan_query_id(<<_Byte, Tail/binary>>) -> scan_query_id(Tail);
scan_query_id(<<>>) -> erlang:error(<<"sentinel missing query_id">>).

value_until_quote(<<$", _Rest/binary>>, Acc) -> Acc;
value_until_quote(<<Byte, Rest/binary>>, Acc) -> value_until_quote(Rest, <<Acc/binary, Byte>>);
value_until_quote(<<>>, _Acc) -> erlang:error(<<"sentinel value unterminated">>).
