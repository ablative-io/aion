/// Schema builders for structured output — mirrors onatopp-dev-norn schema functions.

import gleam/string

pub fn scout_schema() -> String {
  string.concat([
    "{\"type\":\"object\",\"properties\":{",
    "\"summary\":{\"type\":\"string\",\"description\":\"2-3 sentences orienting the implementer.\"},",
    "\"enrichments\":{\"type\":\"array\",\"description\":\"One entry per R#.\",\"items\":{\"type\":\"object\",\"properties\":{",
    "\"id\":{\"type\":\"string\",\"description\":\"R# id from the brief.\"},",
    "\"files\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Key files relevant to this R# (path:line-range — brief note). 2-5 per R#, not exhaustive.\"},",
    "\"context\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Key findings: conventions to match, type signatures, gotchas. 2-4 per R#.\"},",
    "\"approach\":{\"type\":\"string\",\"description\":\"How to implement this R# — one paragraph.\"},",
    "\"notes\":{\"type\":\"string\",\"description\":\"Anything non-obvious: edge cases, integration gotchas, things the brief might not have considered. Empty if none.\"}",
    "},\"required\":[\"id\",\"files\",\"context\",\"approach\",\"notes\"],\"additionalProperties\":false}},",
    "\"verification\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Concrete checks to run after implementation.\"}",
    "},\"required\":[\"summary\",\"enrichments\",\"verification\"],\"additionalProperties\":false}",
  ])
}

pub fn dev_schema() -> String {
  string.concat([
    "{\"type\":\"object\",\"properties\":{",
    "\"summary\":{\"type\":\"string\",\"description\":\"1-2 sentences on what was done.\"},",
    "\"commit_message\":{\"type\":\"string\",\"description\":\"Conventional-commits style.\"},",
    "\"enrichments\":{\"type\":\"array\",\"description\":\"One entry per R#.\",\"items\":{\"type\":\"object\",\"properties\":{",
    "\"id\":{\"type\":\"string\",\"description\":\"R# id.\"},",
    "\"status\":{\"type\":\"string\",\"enum\":[\"implemented\",\"blocked\"]},",
    "\"files_changed\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"properties\":{",
    "\"path\":{\"type\":\"string\"},\"change\":{\"type\":\"string\",\"enum\":[\"created\",\"modified\",\"deleted\"]},\"note\":{\"type\":\"string\"}",
    "},\"required\":[\"path\",\"change\",\"note\"],\"additionalProperties\":false}},",
    "\"how\":{\"type\":\"string\",\"description\":\"How this requirement was met.\"},",
    "\"deviation\":{\"type\":\"string\",\"description\":\"Empty if followed plan. Otherwise: what changed and why.\"},",
    "\"checklist\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"properties\":{\"id\":{\"type\":\"string\"},\"done\":{\"type\":\"boolean\"},\"note\":{\"type\":\"string\"}},\"required\":[\"id\",\"done\",\"note\"],\"additionalProperties\":false}},",
    "\"stories\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"properties\":{\"id\":{\"type\":\"string\"},\"satisfied\":{\"type\":\"boolean\"},\"note\":{\"type\":\"string\"}},\"required\":[\"id\",\"satisfied\",\"note\"],\"additionalProperties\":false}}",
    "},\"required\":[\"id\",\"status\",\"files_changed\",\"how\",\"deviation\",\"checklist\",\"stories\"],\"additionalProperties\":false}},",
    "\"attestation\":{\"type\":\"object\",\"properties\":{",
    "\"no_panics\":{\"type\":\"boolean\",\"description\":\"No unwrap/expect/panic/todo in library code.\"},",
    "\"no_unsafe\":{\"type\":\"boolean\",\"description\":\"No unsafe blocks added.\"},",
    "\"boundaries_respected\":{\"type\":\"boolean\",\"description\":\"All SHALL NOT boundaries observed.\"},",
    "\"tests_pass\":{\"type\":\"boolean\",\"description\":\"cargo check + clippy + test pass on affected crates.\"}",
    "},\"required\":[\"no_panics\",\"no_unsafe\",\"boundaries_respected\",\"tests_pass\"],\"additionalProperties\":false}",
    "},\"required\":[\"summary\",\"commit_message\",\"enrichments\",\"attestation\"],\"additionalProperties\":false}",
  ])
}

pub fn review_schema() -> String {
  string.concat([
    "{\"type\":\"object\",\"properties\":{",
    "\"summary\":{\"type\":\"string\"},",
    "\"commit_message\":{\"type\":\"string\"},",
    "\"pass\":{\"type\":\"boolean\",\"description\":\"True only if all acceptance criteria met and no blocking issues remain.\"},",
    "\"enrichments\":{\"type\":\"array\",\"description\":\"One entry per R#.\",\"items\":{\"type\":\"object\",\"properties\":{",
    "\"id\":{\"type\":\"string\"},",
    "\"alignment\":{\"type\":\"string\",\"enum\":[\"aligned\",\"drifted\",\"fixed\"]},",
    "\"acceptance_met\":{\"type\":\"boolean\"},",
    "\"checklist\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}},",
    "\"stories\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}},",
    "\"issues\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}},",
    "\"fixes\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}}",
    "},\"required\":[\"id\",\"alignment\",\"acceptance_met\",\"checklist\",\"stories\",\"issues\",\"fixes\"],\"additionalProperties\":false}},",
    "\"verification\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"properties\":{",
    "\"criterion\":{\"type\":\"string\"},\"passed\":{\"type\":\"boolean\"},\"note\":{\"type\":\"string\"}",
    "},\"required\":[\"criterion\",\"passed\",\"note\"],\"additionalProperties\":false}}",
    "},\"required\":[\"summary\",\"commit_message\",\"pass\",\"enrichments\",\"verification\"],\"additionalProperties\":false}",
  ])
}
