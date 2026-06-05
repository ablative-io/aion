import aion/codec
import aion/error
import aion/signal

pub type Approval {
  Approval(approved: Bool)
}

fn approval_codec() -> codec.Codec(Approval) {
  codec.Codec(
    encode: fn(_approval) { "{}" },
    decode: fn(_payload) { Ok(Approval(approved: True)) },
  )
}

pub fn valid_signal_payload() -> Result(Approval, error.ReceiveError) {
  let approval_signal = signal.new("approval", approval_codec())
  signal.receive(approval_signal)
}
