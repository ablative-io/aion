//! Deterministic `parse_output` handler.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::Value;

use crate::types::{ParseInput, ParseOutput};

/// Parse text using one of the worker's deterministic data-level modes.
///
/// Invalid input, invalid queries, unsupported modes, and misses are always
/// represented by `ok: false`; this handler never produces an activity failure.
#[must_use]
pub fn parse_output(input: ParseInput) -> ParseOutput {
    let ParseInput { text, mode, query } = input;
    match mode.as_str() {
        "json_path" => parse_json_path(&text, &query),
        "regex" => parse_regex(&text, &query),
        "lines" => parse_lines(&text, &query),
        unsupported => ParseOutput::failure(format!(
            "unsupported parse_output mode `{unsupported}`; expected `json_path`, `regex`, or `lines`"
        )),
    }
}

fn parse_json_path(text: &str, query: &str) -> ParseOutput {
    let root = match serde_json::from_str::<Value>(text) {
        Ok(value) => value,
        Err(source) => {
            return ParseOutput::failure(format!("json_path failed to parse input JSON: {source}"));
        }
    };

    let mut current = &root;
    if !query.is_empty() {
        for (offset, segment) in query.split('.').enumerate() {
            let position = offset + 1;
            if segment.is_empty() {
                return ParseOutput::failure(format!(
                    "json_path segment {position} is empty in query `{query}`"
                ));
            }
            current = match traverse(current, segment, position) {
                Ok(value) => value,
                Err(error) => return ParseOutput::failure(error),
            };
        }
    }

    ParseOutput::success(render_json_value(current))
}

fn traverse<'value>(
    current: &'value Value,
    segment: &str,
    position: usize,
) -> Result<&'value Value, String> {
    match current {
        Value::Object(object) => object.get(segment).ok_or_else(|| {
            format!("json_path key `{segment}` was not found at segment {position}")
        }),
        Value::Array(array) => {
            let index = segment.parse::<usize>().map_err(|source| {
                format!(
                    "json_path segment `{segment}` is not a numeric array index at segment {position}: {source}"
                )
            })?;
            array.get(index).ok_or_else(|| {
                format!("json_path array index {index} is out of bounds at segment {position}")
            })
        }
        scalar => Err(format!(
            "json_path cannot traverse segment `{segment}` through {} at segment {position}",
            value_kind(scalar)
        )),
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn render_json_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn parse_regex(text: &str, query: &str) -> ParseOutput {
    let regex = match Regex::new(query) {
        Ok(regex) => regex,
        Err(source) => {
            return ParseOutput::failure(format!(
                "regex failed to compile query `{query}`: {source}"
            ));
        }
    };
    let Some(captures) = regex.captures(text) else {
        return ParseOutput::failure(format!("regex query `{query}` did not match input"));
    };

    let names: Vec<&str> = regex.capture_names().flatten().collect();
    if !names.is_empty() {
        let mut object = BTreeMap::new();
        for name in names {
            let value = captures
                .name(name)
                .map(|capture| capture.as_str().to_owned());
            object.insert(name.to_owned(), value);
        }
        return serialize_regex_value(&object);
    }

    let mut groups = Vec::new();
    if captures.len() == 1 {
        groups.push(captures.get(0).map(|capture| capture.as_str().to_owned()));
    } else {
        for index in 1..captures.len() {
            groups.push(
                captures
                    .get(index)
                    .map(|capture| capture.as_str().to_owned()),
            );
        }
    }
    serialize_regex_value(&groups)
}

fn serialize_regex_value<T: serde::Serialize>(value: &T) -> ParseOutput {
    match serde_json::to_string(value) {
        Ok(text) => ParseOutput::success(text),
        Err(source) => ParseOutput::failure(format!(
            "regex matched but failed to serialize capture data: {source}"
        )),
    }
}

fn parse_lines(text: &str, query: &str) -> ParseOutput {
    let matching: Vec<&str> = text.lines().filter(|line| line.contains(query)).collect();
    if matching.is_empty() {
        return ParseOutput::failure(format!("lines query `{query}` matched no lines"));
    }
    ParseOutput::success(matching.join("\n"))
}
