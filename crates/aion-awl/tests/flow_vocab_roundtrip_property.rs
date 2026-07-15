//! Property round-trip over the flow-vocabulary B1 syntax: seeded
//! pseudo-random documents mixing raw strings (single- and multi-line),
//! `json { … }` literals (brace-in-string included), `schema of`, const
//! references, `+` concatenations, lists, comments, and trailing comments.
//! For every case: `print(parse(src)) == src` byte-for-byte, the reparse is
//! an identical tree (spans included), and printing is idempotent.

use std::error::Error;
use std::fmt::Write as _;

use aion_awl::{check, parse, print};

type TestResult = Result<(), Box<dyn Error>>;

/// Deterministic xorshift64* generator — no dependencies, stable cases.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

/// A generated const value: its canonical text and whether it is
/// string-typed (only string-typed consts feed concatenations).
struct Value {
    text: String,
    is_string: bool,
}

fn word(rng: &mut Rng) -> String {
    let words = [
        "alpha", "bravo", "carrier", "delta", "notes", "prompt", "verdict", "wave",
    ];
    words[usize::try_from(rng.below(words.len() as u64)).unwrap_or(0)].to_owned()
}

fn string_atom(rng: &mut Rng, string_consts: &[String]) -> String {
    match rng.below(4) {
        0 => format!("\"{} {}\"", word(rng), rng.below(100)),
        1 => format!("\"\"\"{} \"{}\" {}\"\"\"", word(rng), word(rng), word(rng)),
        2 => format!(
            "json {{ \"{}\": \"{{ inner }}\", \"n\": {} }}",
            word(rng),
            rng.below(9)
        ),
        _ => {
            if string_consts.is_empty() {
                "schema of Verdict".to_owned()
            } else {
                let at = usize::try_from(rng.below(string_consts.len() as u64)).unwrap_or(0);
                string_consts[at].clone()
            }
        }
    }
}

fn value(rng: &mut Rng, string_consts: &[String]) -> Value {
    match rng.below(8) {
        0 => Value {
            text: format!(
                "\"\"\"\n  {} line one {}\n\n  {} line two\n  \"\"\"",
                word(rng),
                rng.below(100),
                word(rng)
            ),
            is_string: true,
        },
        1 => Value {
            text: format!(
                "json {{\n  \"{}\": {{ \"brace\": \"{{ not a brace }}\" }},\n  \"count\": {}\n}}",
                word(rng),
                rng.below(50)
            ),
            is_string: true,
        },
        2 => Value {
            text: "schema of Verdict".to_owned(),
            is_string: true,
        },
        3 => {
            let mut text = string_atom(rng, string_consts);
            for _ in 0..=rng.below(2) {
                let _ = write!(text, " + {}", string_atom(rng, string_consts));
            }
            Value {
                text,
                is_string: true,
            }
        }
        4 => Value {
            text: format!("{}", rng.below(10_000)),
            is_string: false,
        },
        5 => Value {
            text: if rng.below(2) == 0 { "true" } else { "false" }.to_owned(),
            is_string: false,
        },
        6 => Value {
            text: format!("[\"{}\", \"{}\"]", word(rng), word(rng)),
            is_string: false,
        },
        _ => Value {
            text: string_atom(rng, string_consts),
            is_string: true,
        },
    }
}

/// Assemble one canonical document with `count` generated consts.
fn document(rng: &mut Rng, case: usize, count: u64) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "//! Property case {case}.");
    let _ = writeln!(out, "workflow prop_case");
    let _ = writeln!(out, "  input task: String");
    let _ = writeln!(out, "  outcome done: type String, route success");
    let _ = writeln!(out);
    let mut string_consts: Vec<String> = Vec::new();
    for index in 0..count {
        if rng.below(4) == 0 {
            let _ = writeln!(out, "// note {index}: {}", word(rng));
        }
        let generated = value(rng, &string_consts);
        let trailing = if rng.below(4) == 0 {
            format!(" // pinned {}", word(rng))
        } else {
            String::new()
        };
        let _ = writeln!(out, "const c{index} = {}{trailing}", generated.text);
        if generated.is_string {
            string_consts.push(format!("c{index}"));
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "type Verdict {{ passed: Bool }}");
    let _ = writeln!(out);
    let _ = writeln!(out, "worker vocab");
    let _ = writeln!(out, "  action stamp(text: String) -> String");
    let _ = writeln!(out);
    let _ = writeln!(out, "step run");
    let _ = writeln!(out, "  task |> stamp |> route done");
    out
}

#[test]
fn generated_vocabulary_documents_round_trip_losslessly() -> TestResult {
    let mut rng = Rng(0x5eed_b1f1_c0de_0001_u64);
    for case in 0..200 {
        let count = 1 + rng.below(6);
        let source = document(&mut rng, case, count);
        let tree = parse(&source)
            .map_err(|error| format!("case {case} failed to parse: {error}\n{source}"))?;
        let printed = print(&tree);
        assert_eq!(
            printed, source,
            "case {case} did not round-trip byte-identically"
        );
        let reparsed = parse(&printed)
            .map_err(|error| format!("case {case} failed to reparse: {error}\n{printed}"))?;
        assert_eq!(
            reparsed, tree,
            "case {case}: parse -> print -> parse changed the tree"
        );
        assert_eq!(
            print(&reparsed),
            printed,
            "case {case}: printing is not idempotent"
        );
        assert!(
            check(&tree).is_empty(),
            "case {case} did not check clean:\n{source}"
        );
    }
    Ok(())
}
