//// Typed query handler registration and replies.

import aion/codec.{type Codec}
import aion/error
import aion/internal/ffi
import gleam/int
import gleam/json
import gleam/string

/// Register a typed read-only query handler with AT's query service.
///
/// The handler's return type is fixed by the `Codec(a)` supplied at
/// registration. When the engine dispatches a query with this name, the SDK
/// invokes `reply`, encodes the returned value with the codec, and replies on the
/// engine-provided one-shot reply channel. Query return values never cross the
/// FFI boundary without a codec.
///
/// Registration stores the encoding handler in the workflow process
/// dictionary (the engine-contract location the yield-point query pump reads
/// from) and then registers the name with the engine. Register before the
/// first yield point — `workflow.sleep`, `workflow.receive`, `workflow.run`
/// — that should answer the query; awaits reached earlier cannot service it.
/// Because workflow code re-executes from the top on replay, re-registration
/// after recovery is automatic: a recovered workflow answers queries without
/// any extra author code.
///
/// Queries bind to AT's read-only query service: they append no workflow `Event`,
/// are answered at engine yield points, and never block workflow progress. By
/// type this callback only returns a value; by workflow-author convention it must
/// not call activity-dispatch primitives such as `workflow.run` or otherwise
/// mutate workflow state — the engine refuses recording calls made while a
/// query is being serviced, surfacing them as a typed handler failure.
pub fn handler(
  name: String,
  value_codec: Codec(value),
  reply: fn() -> value,
) -> Result(Nil, error.QueryError) {
  let encoded_reply = fn(query_id) {
    let encoded = value_codec.encode(reply())
    ffi.reply_query(query_id, encoded)
  }

  ffi.register_query_handler(name, encoded_reply)
  case ffi.register_query(name, register_config()) {
    Ok(_) -> Ok(Nil)
    Error(raw_error) -> Error(query_error(raw_error))
  }
}

/// Dispatch a typed query through the in-engine/client-side binding.
///
/// This helper exists for callers already inside the engine boundary and for the
/// pure Gleam test harness. It asks AT's query service for the named handler,
/// then decodes the encoded reply with the supplied codec. An unregistered name
/// returns `Error(UnknownQuery(name))`; malformed replies return
/// `Error(QueryDecodeFailed(_))`. Query dispatch records no workflow event.
pub fn dispatch(
  name: String,
  value_codec: Codec(value),
) -> Result(value, error.QueryError) {
  case ffi.dispatch_query(name, dispatch_config()) {
    Ok(raw_payload) -> {
      case value_codec.decode(raw_payload) {
        Ok(value) -> Ok(value)
        Error(decode_error) -> Error(error.QueryDecodeFailed(decode_error))
      }
    }
    Error(raw_error) -> Error(query_error(raw_error))
  }
}

/// Return the test/engine observation count reported by the backing query
/// service.
///
/// Production engines may expose this as diagnostic data; the shipped test double
/// uses it to assert that query dispatch does not append recorded observations or
/// history events.
pub fn recorded_observations() -> Result(Int, error.QueryError) {
  case ffi.query_recorded_observations() {
    Ok(raw_count) -> {
      case int.parse(raw_count) {
        Ok(count) -> Ok(count)
        Error(_) ->
          Error(error.QueryEngineFailure("invalid query observation count"))
      }
    }
    Error(raw_error) -> Error(query_error(raw_error))
  }
}

fn register_config() -> String {
  json.object([]) |> json.to_string
}

fn dispatch_config() -> String {
  json.object([]) |> json.to_string
}

fn query_error(raw: String) -> error.QueryError {
  case string.starts_with(raw, "unknown:") {
    True -> error.UnknownQuery(name: string.drop_start(raw, 8))
    False ->
      case string.starts_with(raw, "cancelled:") {
        True ->
          error.QueryCancelled(error.Cancelled(string.drop_start(raw, 10)))
        False ->
          case string.starts_with(raw, "non_determinism:") {
            True ->
              error.QueryNonDeterministic(
                error.NonDeterminismViolation(string.drop_start(raw, 16)),
              )
            False -> error.QueryEngineFailure(raw)
          }
      }
  }
}
