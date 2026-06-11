//// Event stream abstraction with sequence-based resumption.

import aion_client.{type WorkflowHandle}
import aion_client/error.{type Error}
import aion_client/payload
import gleam/dynamic/decode
import gleam/list

pub type Event(event) {
  Event(sequence: Int, payload: event)
}

pub type StreamItem(event) {
  EventItem(event: Event(event))
  StreamError(error: Error)
  StreamEnd
}

pub type EventStream(event) {
  EventStream(read_all: fn() -> List(StreamItem(event)))
}

pub type Frame {
  Frame(sequence: Int, payload: payload.Payload)
  TransientDisconnect
  TerminalFailure(error: Error)
  EndOfStream
}

pub type StubTransport {
  StubTransport(open: fn(Int) -> List(Frame))
}

/// Build a stream for a workflow handle. The concrete WebSocket adapter is an
/// AW-owned transport concern; until that adapter is wired this returns an
/// Unavailable item rather than silently ending.
pub fn subscribe(
  _handle: WorkflowHandle,
  _decoder: decode.Decoder(event),
) -> EventStream(event) {
  EventStream(read_all: fn() { [StreamError(error.Unavailable)] })
}

/// Conformance/test helper that exercises the same cursor protocol as the
/// reference SDK transports: the initial open passes cursor 0 (no events
/// delivered yet), every reconnect after a transient disconnect passes
/// last-delivered sequence + 1, re-sent duplicates are filtered, and a
/// sequence gap surfaces as `Unavailable` instead of silently losing events.
pub fn subscribe_with_stub(
  transport: StubTransport,
  decoder: decode.Decoder(event),
) -> EventStream(event) {
  EventStream(read_all: fn() { read_from_stub(transport, decoder, 0, []) })
}

pub fn collect(stream: EventStream(event)) -> List(StreamItem(event)) {
  let EventStream(read_all: read_all) = stream
  read_all()
}

fn read_from_stub(
  transport: StubTransport,
  decoder: decode.Decoder(event),
  last_delivered: Int,
  delivered: List(StreamItem(event)),
) -> List(StreamItem(event)) {
  let StubTransport(open: open) = transport
  let cursor = case last_delivered {
    0 -> 0
    sequence -> sequence + 1
  }
  let frames = open(cursor)
  read_frames(transport, decoder, frames, last_delivered, delivered)
}

fn read_frames(
  transport: StubTransport,
  decoder: decode.Decoder(event),
  frames: List(Frame),
  last_delivered: Int,
  delivered: List(StreamItem(event)),
) -> List(StreamItem(event)) {
  case frames {
    [] -> reverse(delivered)
    [first, ..rest] ->
      case first {
        Frame(sequence: sequence, payload: raw_payload) -> {
          case sequence <= last_delivered {
            // The server may re-send already-delivered frames after a
            // reconnect; resumption must filter them, never re-deliver.
            True ->
              read_frames(transport, decoder, rest, last_delivered, delivered)
            False ->
              case sequence == last_delivered + 1 {
                // A sequence gap means an event was lost in transit: surface
                // it as an explicit stream error rather than skipping ahead.
                False -> reverse([StreamError(error.Unavailable), ..delivered])
                True ->
                  case payload.decode(raw_payload, decoder) {
                    Ok(event) ->
                      read_frames(transport, decoder, rest, sequence, [
                        EventItem(Event(sequence: sequence, payload: event)),
                        ..delivered
                      ])
                    Error(error) -> reverse([StreamError(error), ..delivered])
                  }
              }
          }
        }
        TransientDisconnect ->
          read_from_stub(transport, decoder, last_delivered, delivered)
        TerminalFailure(error) -> reverse([StreamError(error), ..delivered])
        EndOfStream -> reverse([StreamEnd, ..delivered])
      }
  }
}

fn reverse(items: List(item)) -> List(item) {
  list.reverse(items)
}
