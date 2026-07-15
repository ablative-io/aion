//! Migration fix-its for AWL-0/1 vocabulary that is gone in rev-2: dead
//! keywords and dead type constructors, each mapped to its rev-2
//! replacement.

/// Migration fix-its for AWL-0/1 keywords that are gone in rev-2. The
/// message names both the dead word and its rev-2 replacement.
pub(super) fn gone_keyword_hint(word: &str) -> Option<String> {
    let hint = match word {
        "about" => {
            "rev-2 has no `about`: prose is doc comments — `///` on declarations, `//!` narration at the top"
        }
        "do" => {
            "rev-2 has no `do`: call the action directly and bind with `->` — `action_name(arg: value) -> name`"
        }
        "as" => {
            "rev-2 has no `as`: bind a call's result with `->` — `action_name(arg: value) -> name`"
        }
        "each" => "rev-2 has no `each`: fan out with `fork item in items … join -> name`",
        "repeat" | "up" => {
            "rev-2 has no `repeat`/`up to`: iterate with `loop <name> = <seed> … until <cond> … max <bound>`"
        }
        "finish" => "rev-2 has no `finish`: finishing IS routing — `route <workflow outcome>`",
        "fail" => {
            "rev-2 has no `fail`: route to a failure-mapped outcome — `route <workflow outcome>`"
        }
        "match" | "case" => {
            "rev-2 has no `match`/`case`: branch with outcome clauses — `outcome <name>: when <cond>, route <target>`"
        }
        "parallel" => {
            "rev-2 has no `parallel`: use `fork … join`, or independent steps sharing an `after` dependency"
        }
        "race" => {
            "rev-2 has no `race`: `wait <signal> timeout <duration>` covers signal-or-deadline"
        }
        "output" => "rev-2 has no `output`: declare `outcome <name>: type <Type>, route success`",
        "error" => "rev-2 has no `error`: declare `outcome <name>: type <Type>, route failure`",
        "queue" => {
            "rev-2 has no `queue`: the worker name is the task queue — declare the action in a `worker` block"
        }
        _ => return None,
    };
    Some(hint.to_owned())
}

/// Migration fix-its for AWL-0/1 type constructors that are gone in rev-2.
pub(super) fn gone_type_hint(name: &str) -> Option<String> {
    match name {
        "Option" => {
            Some("rev-2 has no `Option(T)`: optionality is postfix — write `T?`".to_owned())
        }
        "List" => Some("rev-2 has no `List(T)`: the one list spelling is `[T]`".to_owned()),
        _ => None,
    }
}
