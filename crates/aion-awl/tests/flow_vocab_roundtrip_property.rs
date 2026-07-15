//! Property round-trip over the flow-vocabulary syntax. B1: seeded
//! pseudo-random documents mixing raw strings (single- and multi-line),
//! `json { … }` literals (brace-in-string included), `schema of`, const
//! references, `+` concatenations, lists, comments, and trailing comments.
//! B2: documents mixing `subflow` declarations and calls, `distribute`/
//! `sequence` regions, strict and tolerant `collect`s, chained regions,
//! `max … visits` self-cycles with `visits` guards, and value route
//! payloads — comments and trailing comments included. For every case:
//! `print(parse(src)) == src` byte-for-byte, the reparse is an identical
//! tree (spans included), and printing is idempotent.

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

/// Assemble one canonical flow-shape document: an optional subflow (with
/// an invocation step), one or two per-item regions (random verb, random
/// tolerance), optional comments and trailing comments, and an optional
/// `max … visits` self-cycle reading `visits` in a guard.
fn flow_document(rng: &mut Rng, case: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "//! Flow-shape property case {case}.");
    let _ = writeln!(out, "workflow flow_case");
    let _ = writeln!(out, "  input items: [String]");
    let _ = writeln!(out, "  outcome done: type String, route success");
    let _ = writeln!(out);
    let _ = writeln!(out, "worker w");
    let _ = writeln!(out, "  action work(item: String) -> String");
    let with_subflow = rng.below(2) == 0;
    if with_subflow {
        let _ = writeln!(out);
        if rng.below(2) == 0 {
            let _ = writeln!(out, "/// Handle one {}.", word(rng));
        }
        let trailing = if rng.below(2) == 0 {
            format!(" // {}", word(rng))
        } else {
            String::new()
        };
        let _ = writeln!(out, "subflow handle(item: String){trailing}");
        let _ = writeln!(out, "  outcome out: type String");
        let _ = writeln!(out);
        let _ = writeln!(out, "  step run");
        let _ = writeln!(out, "    work(item: item) -> note");
        let _ = writeln!(out);
        let _ = writeln!(out, "    outcome ok: otherwise, route out(note)");
    }
    let regions = 1 + rng.below(2);
    for region in 0..regions {
        let verb = if rng.below(2) == 0 {
            "distribute"
        } else {
            "sequence"
        };
        let tolerant = if rng.below(2) == 0 { "?" } else { "" };
        let _ = writeln!(out);
        if rng.below(3) == 0 {
            let _ = writeln!(out, "// region {region}: {}", word(rng));
        }
        let _ = writeln!(out, "step wave_{region}");
        let trailing = if rng.below(2) == 0 {
            format!(" // {}", word(rng))
        } else {
            String::new()
        };
        let _ = writeln!(out, "  {verb} item_{region} in items{trailing}");
        let _ = writeln!(out);
        let _ = writeln!(out, "step build_{region}");
        if with_subflow && rng.below(2) == 0 {
            let _ = writeln!(out, "  handle(item: item_{region}) -> note_{region}");
        } else {
            let _ = writeln!(out, "  work(item: item_{region}) -> note_{region}");
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "step gather_{region}");
        let trailing = if rng.below(2) == 0 {
            format!(" // {}", word(rng))
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  collect note_{region}{tolerant} -> notes_{region}{trailing}"
        );
    }
    let _ = writeln!(out);
    let last = regions - 1;
    if rng.below(2) == 0 {
        // A visits-bounded self-cycle reading the builtin counter. The
        // self-route makes `tail` route-targeted, so it arms via `after`.
        let _ = writeln!(out, "step tail after gather_{last}");
        let _ = writeln!(
            out,
            "  outcome again: when notes_{last} is empty and visits < 2, route tail"
        );
        let _ = writeln!(out, "  outcome finish: otherwise, route done(\"over\")");
        let _ = writeln!(out, "  max 2 visits");
    } else {
        let _ = writeln!(out, "step tail");
        let _ = writeln!(out, "  \"done\" |> route done");
    }
    out
}

#[test]
fn generated_flow_shape_documents_round_trip_losslessly() -> TestResult {
    let mut rng = Rng(0x5eed_b1f1_c0de_0002_u64);
    for case in 0..150 {
        let source = flow_document(&mut rng, case);
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
        let errors = check(&tree);
        assert!(
            errors.is_empty(),
            "case {case} did not check clean:\n{source}\n{errors:#?}"
        );
    }
    Ok(())
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
