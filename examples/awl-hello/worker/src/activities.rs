//! The two pure computational activity bodies the awl-hello worker serves:
//! `greet` and `shout`.
//!
//! Every type here serializes/deserializes byte-compatibly with the codecs
//! the AWL emitter generates into `../../src/awl_hello.gleam` from the
//! `action` declarations in `../../awl_hello.awl` — those declarations are
//! the authoritative contract (field names in `snake_case`).
//!
//! The bodies are plain synchronous `Input -> Result<Output, _>` so the unit
//! tests drive them directly; `main.rs` adapts them onto the worker's async
//! handler signature.

use aion_worker::ActivityFailure;
use serde::{Deserialize, Serialize};

/// Input to `greet`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GreetInput {
    /// The name to greet.
    pub name: String,
}

/// Result of `greet`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GreetOutput {
    /// The composed greeting: `Hello, <name>!`.
    pub greeting: String,
}

/// Input to `shout`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShoutInput {
    /// The text to shout.
    pub text: String,
}

/// Result of `shout`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShoutOutput {
    /// The input text uppercased, with `!` appended.
    pub text: String,
}

/// `greet`: compose `Hello, <name>!` from the input name.
///
/// # Errors
///
/// Never fails; the `Result` is the worker SDK's handler contract.
pub fn greet(input: GreetInput) -> Result<GreetOutput, ActivityFailure> {
    let GreetInput { name } = input;
    Ok(GreetOutput {
        greeting: format!("Hello, {name}!"),
    })
}

/// `shout`: uppercase the input text and append `!`.
///
/// # Errors
///
/// Never fails; the `Result` is the worker SDK's handler contract.
pub fn shout(input: ShoutInput) -> Result<ShoutOutput, ActivityFailure> {
    let ShoutInput { text } = input;
    let mut text = text.to_uppercase();
    text.push('!');
    Ok(ShoutOutput { text })
}

#[cfg(test)]
mod tests {
    use super::{GreetInput, GreetOutput, ShoutInput, ShoutOutput, greet, shout};

    #[test]
    fn greet_composes_the_greeting() {
        assert_eq!(
            greet(GreetInput {
                name: "Ada".to_owned(),
            }),
            Ok(GreetOutput {
                greeting: "Hello, Ada!".to_owned(),
            })
        );
    }

    #[test]
    fn shout_uppercases_and_appends_a_bang() {
        assert_eq!(
            shout(ShoutInput {
                text: "Hello, Ada!".to_owned(),
            }),
            Ok(ShoutOutput {
                text: "HELLO, ADA!!".to_owned(),
            })
        );
    }

    /// The demo's end-to-end data path: `greet` output feeds `shout` input,
    /// exactly as the placeholder (and later the generated) workflow chains
    /// them.
    #[test]
    fn shout_composes_over_greet() {
        let chained = greet(GreetInput {
            name: "world".to_owned(),
        })
        .and_then(|greeted| {
            shout(ShoutInput {
                text: greeted.greeting,
            })
        });
        assert_eq!(
            chained,
            Ok(ShoutOutput {
                text: "HELLO, WORLD!!".to_owned(),
            })
        );
    }
}
