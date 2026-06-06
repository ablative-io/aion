-module(aion_client_conformance_ffi).
-export([getenv/1, read_file/1]).

getenv(Name) ->
    case os:getenv(binary_to_list(Name)) of
        false -> <<>>;
        Value -> unicode:characters_to_binary(Value)
    end.

read_file(Path) ->
    case file:read_file(binary_to_list(Path)) of
        {ok, Bytes} -> {ok, Bytes};
        {error, Reason} -> {error, unicode:characters_to_binary(io_lib:format("~p", [Reason]))}
    end.
