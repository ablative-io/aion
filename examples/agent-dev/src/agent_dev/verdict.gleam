//// Defensive review-verdict extraction — pure functions only.
////
//// The review agent is INSTRUCTED (in-prompt) to end its terminal text with
//// exactly one JSON object `{"pass": bool, "blockers": [...], "summary":
//// "..."}`, but an agent's output is never trusted: `parse` extracts a
//// trailing JSON object from the text and decodes it with the generated
//// `ReviewVerdict` decoder. Trailing whitespace and a trailing Markdown
//// code fence are tolerated; anything else after the object is not — the
//// workflow's ONE bounded re-ask round handles that, and a still-unparseable
//// reply counts as a failed review round.

import agent_dev_codecs as codecs
import agent_dev_io as io
import gleam/json
import gleam/list
import gleam/result
import gleam/string

/// Extract the trailing JSON verdict from the reviewer's terminal text.
///
/// Strategy: strip trailing whitespace and code-fence backticks, then try
/// every suffix of the text that starts at a `{`, innermost (closest to the
/// end) first. A suffix parses only when it is exactly one JSON object
/// reaching the end of the text, so prose BEFORE the verdict is fine and
/// prose AFTER it is a parse failure by design.
pub fn parse(text: String) -> Result(io.ReviewVerdict, Nil) {
  text
  |> strip_trailing_noise
  |> brace_suffixes
  |> list.find_map(fn(candidate) {
    json.parse(candidate, codecs.review_verdict_decoder())
    |> result.replace_error(Nil)
  })
}

/// Drop trailing whitespace and any trailing Markdown code fence (agents
/// love wrapping JSON in ```), repeating until the text ends in real
/// content.
fn strip_trailing_noise(text: String) -> String {
  let trimmed = string.trim_end(text)
  case string.ends_with(trimmed, "```") {
    True ->
      trimmed
      |> string.drop_end(3)
      |> strip_trailing_noise
    False -> trimmed
  }
}

/// Every suffix of `text` beginning at a `{`, ordered innermost-first (the
/// suffix starting at the LAST `{` comes first). The verdict object's own
/// opening brace is the first suffix that parses as a complete JSON object.
fn brace_suffixes(text: String) -> List(String) {
  case string.split(text, "{") {
    // No `{` at all (`split` returned the whole text as its only part):
    // nothing can be a JSON object.
    [] | [_] -> []
    [_before, ..parts] -> {
      let #(suffixes, _) =
        parts
        |> list.reverse
        |> list.fold(#([], ""), fn(state, part) {
          let #(collected, tail) = state
          let suffix = "{" <> part <> tail
          #(list.append(collected, [suffix]), suffix)
        })
      suffixes
    }
  }
}
