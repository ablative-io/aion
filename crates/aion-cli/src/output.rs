//! JSON output helpers for the CLI.

use std::io::{self, Write};

use aion_client::{ReopenOutcome, WorkflowHandle};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Map, Value, json};

#[derive(Serialize)]
pub(crate) struct StartOutput {
    workflow_id: String,
    run_id: String,
}

#[derive(Serialize)]
pub(crate) struct AcknowledgementOutput<'a> {
    pub(crate) workflow_id: &'a str,
    pub(crate) accepted: bool,
}

#[derive(Serialize)]
pub(crate) struct QueryOutput {
    pub(crate) result: Value,
}

#[derive(Serialize)]
pub(crate) struct ReopenOutput {
    pub(crate) workflow_id: String,
    pub(crate) run_id: String,
    pub(crate) status: String,
    pub(crate) reopened: bool,
}

#[derive(Serialize)]
pub(crate) struct DescribeOutput<TSummary, THistory> {
    pub(crate) summary: TSummary,
    pub(crate) history: THistory,
}

pub(crate) fn start_output(handle: &WorkflowHandle) -> StartOutput {
    StartOutput {
        workflow_id: handle.workflow_id().to_string(),
        run_id: handle.run_id().to_string(),
    }
}

pub(crate) fn reopen_output(workflow_id: &str, outcome: &ReopenOutcome) -> ReopenOutput {
    ReopenOutput {
        workflow_id: workflow_id.to_owned(),
        run_id: outcome.run_id.to_string(),
        status: format!("{:?}", outcome.status),
        reopened: true,
    }
}

pub(crate) fn to_value<T>(value: T) -> Result<Value>
where
    T: Serialize,
{
    serde_json::to_value(value).context("failed to encode command output")
}

pub(crate) fn describe_output<TSummary, THistory>(
    summary: TSummary,
    history: THistory,
    raw: bool,
) -> Result<Value>
where
    TSummary: Serialize,
    THistory: Serialize,
{
    let mut value = to_value(DescribeOutput { summary, history })?;
    if !raw {
        decode_payloads_in_history(&mut value);
    }
    Ok(value)
}

fn decode_payloads_in_history(value: &mut Value) {
    if let Value::Object(object) = value {
        if let Some(history) = object.get_mut("history") {
            decode_payloads_in_value(history);
        }
    }
}

fn decode_payloads_in_value(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                decode_payloads_in_value(item);
            }
        }
        Value::Object(object) => {
            if let Some(display_value) = payload_display_value(object) {
                *value = display_value;
            } else {
                for item in object.values_mut() {
                    decode_payloads_in_value(item);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn payload_display_value(object: &Map<String, Value>) -> Option<Value> {
    let content_type = object.get("content_type")?.as_str()?;
    let bytes = payload_bytes(object.get("bytes")?)?;

    if bytes.is_empty() {
        return Some(json!({
            "content_type": content_type,
            "empty": true
        }));
    }

    if content_type == "Json" {
        if let Ok(decoded) = serde_json::from_slice::<Value>(&bytes) {
            return Some(decoded);
        }
    }

    Some(json!({
        "content_type": content_type,
        "encoding": "hex",
        "data": hex_encode(&bytes)
    }))
}

fn payload_bytes(value: &Value) -> Option<Vec<u8>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(crate) fn print_json(value: &Value, pretty: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if pretty {
        serde_json::to_writer_pretty(&mut handle, value).context("failed to write JSON output")?;
    } else {
        serde_json::to_writer(&mut handle, value).context("failed to write JSON output")?;
    }
    writeln!(handle).context("failed to write trailing newline")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{decode_payloads_in_value, describe_output};

    #[test]
    fn describe_output_decodes_json_payloads_recursively() -> anyhow::Result<()> {
        let history = json!([
            {
                "event_type": "ActivityScheduled",
                "input": payload(r#"{"activity":"sync","nested":{"count":2}}"#)
            },
            {
                "event_type": "ActivityCompleted",
                "result": payload(r#"[true,{"done":false}]"#)
            },
            {
                "event_type": "WorkflowCompleted",
                "result": payload(r#""finished""#)
            }
        ]);

        let output = describe_output(json!({ "workflow_id": "wf" }), history, false)?;

        assert_eq!(
            output["history"][0]["input"],
            json!({"activity": "sync", "nested": {"count": 2}})
        );
        assert_eq!(
            output["history"][1]["result"],
            json!([true, {"done": false}])
        );
        assert_eq!(output["history"][2]["result"], json!("finished"));
        assert_eq!(output["summary"]["workflow_id"], json!("wf"));
        Ok(())
    }

    #[test]
    fn raw_describe_output_preserves_payload_byte_arrays() -> anyhow::Result<()> {
        let history = json!([{ "input": payload(r#"{"value":1}"#) }]);

        let output = describe_output(json!({ "workflow_id": "wf" }), history, true)?;

        assert_eq!(output["history"][0]["input"], payload(r#"{"value":1}"#));
        Ok(())
    }

    #[test]
    fn describe_output_only_decodes_payloads_inside_history() -> anyhow::Result<()> {
        let summary = json!({
            "workflow_id": "wf",
            "metadata": payload(r#"{"should":"remain raw"}"#)
        });
        let history = json!([{ "input": payload(r#"{"should":"decode"}"#) }]);

        let output = describe_output(summary, history, false)?;

        assert_eq!(
            output["summary"]["metadata"],
            payload(r#"{"should":"remain raw"}"#)
        );
        assert_eq!(output["history"][0]["input"], json!({ "should": "decode" }));
        Ok(())
    }

    #[test]
    fn malformed_json_payload_falls_back_to_hex() {
        let mut value = json!({
            "payload": {
                "content_type": "Json",
                "bytes": [123, 110, 111, 116]
            }
        });

        decode_payloads_in_value(&mut value);

        assert_eq!(
            value["payload"],
            json!({
                "content_type": "Json",
                "encoding": "hex",
                "data": "7b6e6f74"
            })
        );
    }

    #[test]
    fn invalid_utf8_json_payload_falls_back_to_hex() {
        let mut value = json!({
            "payload": {
                "content_type": "Json",
                "bytes": [255, 254, 253]
            }
        });

        decode_payloads_in_value(&mut value);

        assert_eq!(
            value["payload"],
            json!({
                "content_type": "Json",
                "encoding": "hex",
                "data": "fffefd"
            })
        );
    }

    #[test]
    fn non_json_payload_falls_back_to_hex_with_content_type() {
        let mut value = json!({
            "payload": {
                "content_type": "Binary",
                "bytes": [0, 15, 16, 255]
            }
        });

        decode_payloads_in_value(&mut value);

        assert_eq!(
            value["payload"],
            json!({
                "content_type": "Binary",
                "encoding": "hex",
                "data": "000f10ff"
            })
        );
    }

    #[test]
    fn empty_payload_uses_clear_empty_indicator() {
        let mut value = json!({
            "payload": {
                "content_type": "Json",
                "bytes": []
            }
        });

        decode_payloads_in_value(&mut value);

        assert_eq!(
            value["payload"],
            json!({
                "content_type": "Json",
                "empty": true
            })
        );
    }

    #[test]
    fn invalid_payload_shape_is_left_unchanged() {
        let original = json!({
            "content_type": "Json",
            "bytes": [256]
        });
        let mut value = original.clone();

        decode_payloads_in_value(&mut value);

        assert_eq!(value, original);
    }

    fn payload(json: &str) -> serde_json::Value {
        json!({
            "content_type": "Json",
            "bytes": json.as_bytes()
        })
    }
}
