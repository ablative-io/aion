//// Pins the Gleam side of the remote-provision wire extension (#175): the
//// provision-input codec round-trips `run_id` (the workflow execution's
//// unique id the workflow threads from `workflow.id()` into
//// `ProvisionInput`), omits absent optional fields from the wire entirely,
//// and still decodes older payloads that carry neither `clone_url` nor
//// `run_id`.

import aion/codec
import gleam/option
import gleam/string
import gleeunit/should
import stacked_dev/codecs_core
import stacked_dev/types.{
  type ProvisionInput, Copy, Local, ProvisionInput, Remote, Worktree,
}

fn remote_input() -> ProvisionInput {
  ProvisionInput(
    repo_root: "/abs/repo",
    brief_id: "brief-7",
    base_ref: "main",
    placement: Remote,
    isolation: Copy,
    clone_url: option.Some("git@example.com:repo.git"),
    run_id: option.Some("8b9e6a2d-run"),
  )
}

pub fn provision_input_codec_round_trips_run_id_test() {
  let codec.Codec(encode: encode, decode: decode) =
    codecs_core.provision_input_codec()
  let input = remote_input()
  let wire = encode(input)
  wire
  |> string.contains("\"run_id\":\"8b9e6a2d-run\"")
  |> should.be_true
  wire
  |> string.contains("\"clone_url\":\"git@example.com:repo.git\"")
  |> should.be_true
  decode(wire)
  |> should.equal(Ok(input))
}

pub fn provision_input_codec_omits_absent_remote_fields_test() {
  let codec.Codec(encode: encode, decode: decode) =
    codecs_core.provision_input_codec()
  let input =
    ProvisionInput(
      repo_root: "/abs/repo",
      brief_id: "brief-7",
      base_ref: "main",
      placement: Local,
      isolation: Worktree,
      clone_url: option.None,
      run_id: option.None,
    )
  let wire = encode(input)
  wire
  |> string.contains("run_id")
  |> should.be_false
  wire
  |> string.contains("clone_url")
  |> should.be_false
  decode(wire)
  |> should.equal(Ok(input))
}
