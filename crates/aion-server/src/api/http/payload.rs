//! HTTP body/payload encode-decode shapes and conversions.

use aion_core::DescribeWorkflowResponse;
use aion_proto::{ProtoDescribeWorkflowResponse, WireEnvelope, WireError};
use serde_json::Value;

use super::error::HttpWireError;

pub(crate) const JSON_CONTENT_TYPE: &str = "application/json";

pub(super) fn http_input_payload(
    input: Value,
) -> Result<aion_proto::convert::ProtoPayload, WireError> {
    if is_payload_envelope(&input) {
        serde_json::from_value(input).map_err(|_error| invalid_start_input())
    } else {
        serde_json::to_vec(&input)
            .map(|bytes| aion_proto::convert::ProtoPayload {
                content_type: JSON_CONTENT_TYPE.to_owned(),
                bytes,
            })
            .map_err(|_error| invalid_start_input())
    }
}

fn is_payload_envelope(input: &Value) -> bool {
    input
        .as_object()
        .is_some_and(|object| object.contains_key("content_type") && object.contains_key("bytes"))
}

fn invalid_start_input() -> WireError {
    WireError::invalid_input(
        "start workflow request must be JSON shaped like \
         {\"namespace\":\"tenant-a\",\"workflow_type\":\"example\",\"input\":{\"name\":\"Ada\"}} \
         or {\"namespace\":\"tenant-a\",\"workflow_type\":\"example\",\"input\":{\"content_type\":\"application/json\",\"bytes\":[123,125]}}",
    )
}

/// Convert the proto describe response into the dashboard-facing
/// [`DescribeWorkflowResponse`]: the summary is decoded into the generated
/// [`aion_core::WorkflowSummary`] shape and each history envelope is decoded
/// into a plain [`aion_core::Event`], so the wire matches the generated
/// TypeScript bindings field-for-field (no protobuf-derived `{content_type,
/// data}` payload wrappers).
pub(crate) fn describe_response_to_dashboard(
    response: &ProtoDescribeWorkflowResponse,
) -> Result<DescribeWorkflowResponse, HttpWireError> {
    let summary = response
        .summary
        .as_ref()
        .map(decode_summary_envelope)
        .transpose()?;
    let history = response
        .history
        .iter()
        .map(decode_event_envelope)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DescribeWorkflowResponse { summary, history })
}

fn decode_summary_envelope(
    envelope: &WireEnvelope,
) -> Result<aion_core::WorkflowSummary, HttpWireError> {
    aion_proto::decode_core_value::<aion_core::WorkflowSummary>(envelope).map_err(HttpWireError)
}

fn decode_event_envelope(envelope: &WireEnvelope) -> Result<aion_core::Event, HttpWireError> {
    aion_proto::decode_event(envelope).map_err(HttpWireError)
}

#[cfg(test)]
mod tests {
    use aion_proto::WireErrorCode;
    use serde_json::json;

    use super::*;

    #[test]
    fn http_start_input_normalization_accepts_plain_json_and_legacy_envelope()
    -> Result<(), Box<dyn std::error::Error>> {
        let plain = http_input_payload(json!({ "name": "Ada" }))?;
        assert_eq!(plain.content_type, JSON_CONTENT_TYPE);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&plain.bytes)?,
            json!({ "name": "Ada" })
        );

        let envelope = json!({
            "content_type": "application/json; charset=utf-8",
            "bytes": [123, 34, 110, 97, 109, 101, 34, 58, 34, 65, 100, 97, 34, 125],
        });
        let legacy = http_input_payload(envelope)?;
        assert_eq!(legacy.content_type, "application/json; charset=utf-8");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&legacy.bytes)?,
            json!({ "name": "Ada" })
        );

        let malformed = http_input_payload(
            json!({ "content_type": "application/json", "bytes": "not-a-byte-array" }),
        );
        assert!(matches!(malformed, Err(error) if error.code == WireErrorCode::InvalidInput));

        Ok(())
    }
}
