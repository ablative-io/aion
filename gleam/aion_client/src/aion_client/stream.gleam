//// Event stream abstraction with sequence-based resumption.

import aion_client.{type WorkflowHandle}
import aion_client/error.{type Error}
import aion_client/payload
import aion_client/payload.{type Payload}
import gleam/dynamic/decode
import gleam/list
import gleam/option.{type Option, None, Some}

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
  Frame(sequence: Int, payload: Payload)
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

/// Conformance/test helper that exercises the same cursor logic as the
/// WebSocket implementation: after a transient disconnect it reopens from
/// last-delivered sequence + 1 and filters duplicates.
pub fn subscribe_with_stub(
  transport: StubTransport,
  decoder: decode.Decoder(event),
) -> EventStream(event) {
  EventStream(read_all: fn() { read_from_stub(transport, decoder, 0, None, []) })
}

pub fn collect(stream: EventStream(event)) -> List(StreamItem(event)) {
  let EventStream(read_all: read_all) = stream
  read_all()
}

fn read_from_stub(
  transport: StubTransport,
  decoder: decode.Decoder(event),
  next_sequence: Int,
  last_delivered: Option(Int),
  delivered: List(StreamItem(event)),
) -> List(StreamItem(event)) {
  let StubTransport(open: open) = transport
  let frames = open(next_sequence)
  read_frames(transport, decoder, frames, next_sequence, last_delivered, delivered)
}

fn read_frames(
  transport: StubTransport,
  decoder: decode.Decoder(event),
  frames: List(Frame),
  next_sequence: Int,
  last_delivered: Option(Int),
  delivered: List(StreamItem(event)),
) -> List(StreamItem(event)) {
  case frames {
    [] -> reverse(delivered)
    [first, ..rest] ->
      case first {
        Frame(sequence: sequence, payload: raw_payload) -> {
          let duplicate = case last_delivered {
            Some(last) -> sequence <= last
            None -> False
          }

          case duplicate {
            True ->
              read_frames(
                transport,
                decoder,
                rest,
                next_sequence,
                last_delivered,
                delivered,
              )
            False ->
              case payload.decode(raw_payload, decoder) {
                Ok(event) ->
                  read_frames(
                    transport,
                    decoder,
                    rest,
                    sequence + 1,
                    Some(sequence),
                    [EventItem(Event(sequence: sequence, payload: event)), ..delivered],
                  )
                Error(error) -> reverse([StreamError(error), ..delivered])
              }
          }
        }
        TransientDisconnect ->
          read_from_stub(transport, decoder, next_sequence, last_delivered, delivered)
        TerminalFailure(error) -> reverse([StreamError(error), ..delivered])
        EndOfStream -> reverse([StreamEnd, ..delivered])
      }
  }
}

fn reverse(items: List(item)) -> List(item) {
  list.reverse(items)
}
