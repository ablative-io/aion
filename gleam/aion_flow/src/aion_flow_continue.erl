%% Native helper for aion/workflow/continue.gleam.
-module(aion_flow_continue).
-export([encode/1]).

encode(Input) ->
    unicode:characters_to_binary(json(Input)).

json(Value) when Value =:= nil ->
    "null";
json(Value) when Value =:= true ->
    "true";
json(Value) when Value =:= false ->
    "false";
json(Value) when is_integer(Value) ->
    integer_to_list(Value);
json(Value) when is_float(Value) ->
    float_to_list(Value, [{decimals, 16}, compact]);
json(Value) when is_binary(Value) ->
    quote(Value);
json(Value) when is_list(Value) ->
    [$[, join([json(Item) || Item <- Value], $,), $]];
json(Value) when is_map(Value) ->
    Pairs = maps:to_list(Value),
    [$\{, join([pair(Key, Item) || {Key, Item} <- Pairs], $,), $\}];
json(Value) ->
    quote(unicode:characters_to_binary(io_lib:format("~tp", [Value]))).

pair(Key, Value) ->
    [quote(key_binary(Key)), $:, json(Value)].

key_binary(Key) when is_binary(Key) ->
    Key;
key_binary(Key) when is_atom(Key) ->
    atom_to_binary(Key, utf8);
key_binary(Key) when is_integer(Key) ->
    integer_to_binary(Key);
key_binary(Key) ->
    unicode:characters_to_binary(io_lib:format("~tp", [Key])).

join([], _Separator) ->
    [];
join([Item], _Separator) ->
    Item;
join([Item | Rest], Separator) ->
    [Item, Separator, join(Rest, Separator)].

quote(Value) ->
    [$", escape(unicode:characters_to_list(Value)), $"].

escape([]) ->
    [];
escape([$" | Rest]) ->
    [$\\, $" | escape(Rest)];
escape([$\\ | Rest]) ->
    [$\\, $\\ | escape(Rest)];
escape([$\b | Rest]) ->
    [$\\, $b | escape(Rest)];
escape([$\f | Rest]) ->
    [$\\, $f | escape(Rest)];
escape([$\n | Rest]) ->
    [$\\, $n | escape(Rest)];
escape([$\r | Rest]) ->
    [$\\, $r | escape(Rest)];
escape([$\t | Rest]) ->
    [$\\, $t | escape(Rest)];
escape([Codepoint | Rest]) when Codepoint < 16#20 ->
    [io_lib:format("\\u~4.16.0B", [Codepoint]) | escape(Rest)];
escape([Codepoint | Rest]) ->
    [Codepoint | escape(Rest)].
