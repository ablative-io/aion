//! HTTP body/payload encode-decode shapes and conversions.

use aion_proto::{
    ProtoDescribeWorkflowResponse, ProtoStartWorkflowRequest, WireEnvelope, WireError,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::error::HttpWireError;

pub(crate) const JSON_CONTENT_TYPE: &str = "application/json";

#[derive(Debug, Deserialize)]
struct HttpStartWorkflowRequest {
    namespace: String,
    workflow_type: String,
    input: Option<Value>,
    /// R-4 steered-start routing key (optional; absent keeps unsteered placement).
    #[serde(default)]
    routing_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HttpDescribeWorkflowResponse {
    summary: Option<HttpEnvelope>,
    history: Vec<HttpEnvelope>,
}

#[derive(Debug, Serialize)]
struct HttpEnvelope {
    namespace: String,
    request_id: Option<String>,
    payload: Option<HttpPayload>,
}

#[derive(Debug, Serialize)]
struct HttpPayload {
    content_type: String,
    data: Value,
}

pub(crate) fn decode_start_workflow_request(
    body: &[u8],
) -> Result<ProtoStartWorkflowRequest, HttpWireError> {
    serde_json::from_slice::<HttpStartWorkflowRequest>(body)
        .map_err(|_error| HttpWireError(invalid_start_input()))?
        .try_into()
        .map_err(HttpWireError)
}

impl TryFrom<HttpStartWorkflowRequest> for ProtoStartWorkflowRequest {
    type Error = WireError;

    fn try_from(request: HttpStartWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_type: request.workflow_type,
            input: request.input.map(http_input_payload).transpose()?,
            routing_key: request.routing_key,
        })
    }
}

fn http_input_payload(input: Value) -> Result<aion_proto::convert::ProtoPayload, WireError> {
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

impl TryFrom<ProtoDescribeWorkflowResponse> for HttpDescribeWorkflowResponse {
    type Error = HttpWireError;

    fn try_from(response: ProtoDescribeWorkflowResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            summary: response.summary.map(HttpEnvelope::try_from).transpose()?,
            history: response
                .history
                .into_iter()
                .map(HttpEnvelope::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl TryFrom<WireEnvelope> for HttpEnvelope {
    type Error = HttpWireError;

    fn try_from(envelope: WireEnvelope) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: envelope.namespace,
            request_id: envelope.request_id,
            payload: envelope.payload.map(HttpPayload::try_from).transpose()?,
        })
    }
}

impl TryFrom<aion_proto::convert::ProtoPayload> for HttpPayload {
    type Error = HttpWireError;

    fn try_from(payload: aion_proto::convert::ProtoPayload) -> Result<Self, Self::Error> {
        let content_type = payload.content_type;
        Ok(Self {
            data: payload_data(&content_type, &payload.bytes)?,
            content_type,
        })
    }
}

fn http_payload_content_type(content_type: &str) -> &str {
    if content_type == "Json" {
        JSON_CONTENT_TYPE
    } else {
        content_type
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let normalized = http_payload_content_type(content_type);
    normalized
        .split_once(';')
        .map_or(normalized, |(media_type, _parameters)| media_type)
        .trim()
        .eq_ignore_ascii_case(JSON_CONTENT_TYPE)
}

fn payload_data(content_type: &str, bytes: &[u8]) -> Result<Value, HttpWireError> {
    if is_json_content_type(content_type) {
        let value = serde_json::from_slice(bytes).map_err(|_error| {
            HttpWireError(WireError::backend(
                "application/json payload contains invalid JSON",
            ))
        })?;
        rewrite_payload_values(value)
    } else {
        Ok(Value::String(BASE64_STANDARD.encode(bytes)))
    }
}

fn rewrite_payload_values(value: Value) -> Result<Value, HttpWireError> {
    match value {
        Value::Array(values) => values
            .into_iter()
            .map(rewrite_payload_values)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object)
            if object.contains_key("content_type") && object.contains_key("bytes") =>
        {
            rewrite_payload_object(object)
        }
        Value::Object(object) => object
            .into_iter()
            .map(|(key, value)| rewrite_payload_values(value).map(|value| (key, value)))
            .collect::<Result<Map<_, _>, _>>()
            .map(Value::Object),
        scalar => Ok(scalar),
    }
}

fn rewrite_payload_object(object: Map<String, Value>) -> Result<Value, HttpWireError> {
    let mut payload: aion_proto::convert::ProtoPayload =
        serde_json::from_value(Value::Object(object)).map_err(|_error| {
            HttpWireError(WireError::backend("stored payload envelope is malformed"))
        })?;
    if payload.content_type == "Json" {
        JSON_CONTENT_TYPE.clone_into(&mut payload.content_type);
    }
    let payload = HttpPayload::try_from(payload)?;
    Ok(json!({
        "content_type": payload.content_type,
        "data": payload.data,
    }))
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

    #[test]
    fn http_payload_base64_encodes_non_json_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let data = payload_data("application/octet-stream", &[0, 1, 2])
            .map_err(|error| std::io::Error::other(error.0.message))?;
        assert_eq!(data, json!("AAEC"));
        Ok(())
    }

    #[test]
    fn http_payload_decodes_json_content_type_with_parameters()
    -> Result<(), Box<dyn std::error::Error>> {
        let data = payload_data("application/json; charset=utf-8", br#"{"name":"Ada"}"#)
            .map_err(|error| std::io::Error::other(error.0.message))?;
        assert_eq!(data, json!({ "name": "Ada" }));
        Ok(())
    }
}
