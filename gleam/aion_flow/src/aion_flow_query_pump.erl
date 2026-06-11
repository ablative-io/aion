%% Workflow-side query pump runtime for aion_flow.
%%
%% The engine answers workflow queries at yield points: when a query is
%% pending, each suspending await returns the sentinel
%% `{error, <<"aion_query:", Json/binary>>}` with
%% `Json = {"query_id":"<uuid>","name":"<query name>"}` instead of resolving.
%% The SDK pump loop (`aion/internal/pump`) hands the JSON payload to
%% `service/1`, which decodes it, fetches the handler fun the SDK stored in
%% the process dictionary under `{aion_query_handler, Name}` (the engine
%% contract key; GC-rooted, unlike Rust-held terms), and applies
%% `Handler(QueryId)` inside try/catch. On a raise, or when no handler is
%% registered, the failure is replied through
%% `aion_flow_ffi:reply_query_error/2` so the caller observes a typed
%% handler failure.
%%
%% This module is plain Erlang because it is the only place in the package
%% that can catch exceptions: a query handler raise must NEVER crash the
%% workflow process, and reply failures (a late reply after the caller timed
%% out) are non-fatal by design. `service/1` always returns normally.
%%
%% `aion_flow_ffi` resolves to the engine's NIF registry namespace in
%% production and to the in-process test double under `gleam test`; this
%% module is byte-identical in both environments and must only use Erlang
%% constructs the beamr VM executes (no OTP library modules).
-module(aion_flow_query_pump).

-export([register/2, service/1]).

%% Store a query handler fun in the workflow process dictionary under the
%% engine-contract key `{aion_query_handler, Name}`, where the engine's
%% yield-point pump expects it. Gleam never touches raw process-dictionary
%% types; `aion/internal/ffi` binds this helper instead. Returns `nil`
%% (Gleam's `Nil`).
register(Name, Handler) when is_binary(Name), is_function(Handler, 1) ->
    erlang:put({aion_query_handler, Name}, Handler),
    nil.

%% Service one query sentinel payload (the JSON binary after the
%% `aion_query:` prefix). Always returns `nil` (Gleam's `Nil`) — never
%% raises, whatever the handler does.
service(Payload) when is_binary(Payload) ->
    case scan_query_id(Payload) of
        {ok, QueryId} ->
            case scan_name(Payload) of
                {ok, Name} ->
                    run_handler(QueryId, Name);
                error ->
                    reply_error(
                        QueryId,
                        <<"malformed query sentinel: missing name field in ", Payload/binary>>
                    )
            end;
        error ->
            %% Without a query id there is no reply channel to fail onto;
            %% the engine-side caller observes its configured timeout. The
            %% pump must keep the workflow alive regardless.
            nil
    end.

run_handler(QueryId, Name) ->
    case erlang:get({aion_query_handler, Name}) of
        undefined ->
            reply_error(
                QueryId,
                <<"no handler registered in the process dictionary for query ", Name/binary>>
            );
        Handler when is_function(Handler, 1) ->
            try Handler(QueryId) of
                %% The handler replies through `reply_query` itself and
                %% returns that result. A failed reply ({error, _}: late
                %% reply after caller timeout, unknown query id) is
                %% non-fatal — the caller already stopped waiting.
                _ReplyOutcome -> nil
            catch
                Class:Reason ->
                    reply_error(QueryId, describe_exception(Class, Reason))
            end;
        _NotAFun ->
            reply_error(
                QueryId,
                <<"registered query handler for ", Name/binary, " is not a unary fun">>
            )
    end.

%% Send a handler-failure reply. Reply failures are swallowed deliberately:
%% the reply path runs outside the handler try/catch, and the pump's
%% never-crash-the-workflow invariant must not depend on the FFI boundary's
%% own error contract (a late error reply after the caller timed out comes
%% back as `{error, <<"unknown_query_id:...">>}` and means nobody is
%% listening any more).
reply_error(QueryId, Message) ->
    try aion_flow_ffi:reply_query_error(QueryId, Message) of
        {ok, _Confirmation} -> nil;
        {error, _Reason} -> nil
    catch
        _Class:_Reason -> nil
    end.

describe_exception(Class, Reason) ->
    <<"query handler raised ", (atom_to_binary(Class, utf8))/binary, ": ",
        (describe_reason(Reason))/binary>>.

%% Render an exception reason as readable text without OTP formatting
%% helpers (io_lib is not part of the beamr-executable surface). Gleam
%% `panic`/`let assert` reasons are maps carrying a binary `message`.
describe_reason(Reason) when is_binary(Reason) ->
    Reason;
describe_reason(Reason) when is_atom(Reason) ->
    atom_to_binary(Reason, utf8);
describe_reason(#{message := Message}) when is_binary(Message) ->
    Message;
describe_reason(Reason) when is_tuple(Reason), tuple_size(Reason) > 0 ->
    case element(1, Reason) of
        Tag when is_atom(Tag) -> atom_to_binary(Tag, utf8);
        _NotAnAtom -> <<"unrecognised exception term">>
    end;
describe_reason(_Reason) ->
    <<"unrecognised exception term">>.

%% --- sentinel JSON field extraction -------------------------------------
%%
%% The engine emits the sentinel JSON with serde_json: compact encoding,
%% exactly the string fields `query_id` and `name`, with `"` and `\` inside
%% values always escaped. A raw `"query_id":"` / `"name":"` byte sequence
%% therefore never occurs inside a value, so scanning for the literal key
%% pattern is exact. Key order is irrelevant to the scan.

scan_query_id(<<"\"query_id\":\"", Rest/binary>>) -> unescape(Rest, <<>>);
scan_query_id(<<_Byte, Tail/binary>>) -> scan_query_id(Tail);
scan_query_id(<<>>) -> error.

scan_name(<<"\"name\":\"", Rest/binary>>) -> unescape(Rest, <<>>);
scan_name(<<_Byte, Tail/binary>>) -> scan_name(Tail);
scan_name(<<>>) -> error.

%% Decode a JSON string value up to its closing quote, resolving the escape
%% sequences serde_json emits (`\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`,
%% `\t`, and `\uXXXX` for control characters). Unterminated or malformed
%% input yields `error`.
unescape(<<>>, _Acc) -> error;
unescape(<<$", _Rest/binary>>, Acc) -> {ok, Acc};
unescape(<<$\\, Rest/binary>>, Acc) -> unescape_escaped(Rest, Acc);
unescape(<<Byte, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, Byte>>).

unescape_escaped(<<$", Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $">>);
unescape_escaped(<<$\\, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $\\>>);
unescape_escaped(<<$/, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $/>>);
unescape_escaped(<<$b, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $\b>>);
unescape_escaped(<<$f, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $\f>>);
unescape_escaped(<<$n, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $\n>>);
unescape_escaped(<<$r, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $\r>>);
unescape_escaped(<<$t, Rest/binary>>, Acc) -> unescape(Rest, <<Acc/binary, $\t>>);
unescape_escaped(<<$u, H1, H2, H3, H4, Rest/binary>>, Acc) ->
    case hex_codepoint(H1, H2, H3, H4) of
        {ok, Codepoint} -> unescape(Rest, <<Acc/binary, Codepoint/utf8>>);
        error -> error
    end;
unescape_escaped(_Other, _Acc) ->
    error.

%% serde_json only `\u`-escapes control characters (< U+0020), never
%% surrogate pairs, so a lone Basic Multilingual Plane codepoint outside the
%% surrogate range is the complete valid input space.
hex_codepoint(H1, H2, H3, H4) ->
    case {hex_value(H1), hex_value(H2), hex_value(H3), hex_value(H4)} of
        {{ok, V1}, {ok, V2}, {ok, V3}, {ok, V4}} ->
            Codepoint = V1 * 4096 + V2 * 256 + V3 * 16 + V4,
            case Codepoint >= 16#D800 andalso Codepoint =< 16#DFFF of
                true -> error;
                false -> {ok, Codepoint}
            end;
        _Invalid ->
            error
    end.

hex_value(Char) when Char >= $0, Char =< $9 -> {ok, Char - $0};
hex_value(Char) when Char >= $a, Char =< $f -> {ok, Char - $a + 10};
hex_value(Char) when Char >= $A, Char =< $F -> {ok, Char - $A + 10};
hex_value(_Char) -> error.
