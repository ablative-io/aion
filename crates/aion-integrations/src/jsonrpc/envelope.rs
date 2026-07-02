//! The generic JSON-RPC 2.0 envelope types and the standard error codes.
//!
//! Lifted and generalised from Norn's in-tree MCP prior art (§9.4): the same
//! `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError` envelopes and `-32700/-32600/-32601/-32603`
//! error codes, with the MCP-specific payloads dropped so the envelopes are harness-neutral.

use serde::{Deserialize, Serialize};

/// The JSON-RPC version string every envelope carries.
pub const JSONRPC_VERSION: &str = "2.0";

/// The standard JSON-RPC 2.0 error codes.
///
/// The reserved implementation range `-32000..=-32099` is left to the concrete adapter (e.g. an
/// application `stale target` code); only the protocol-standard codes live here.
pub mod error_codes {
    /// Invalid JSON was received (the payload could not be parsed).
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The requested method does not exist or is not available on this peer.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A JSON-RPC request or response id.
///
/// JSON-RPC 2.0 permits a string, a number, or null as an id. Modelled as an untagged enum so it
/// round-trips any peer's id shape verbatim and can be compared for correlation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum JsonRpcId {
    /// A numeric id (the shape this layer allocates for its own requests).
    Number(u64),
    /// A string id (accepted from a peer that allocates string ids).
    Text(String),
}

impl JsonRpcId {
    /// A numeric id.
    #[must_use]
    pub const fn number(value: u64) -> Self {
        Self::Number(value)
    }
}

/// A JSON-RPC 2.0 request: a call correlated by its `id`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JsonRpcRequest {
    /// The JSON-RPC version, always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The correlation id. A request always carries one (a notification does not).
    pub id: JsonRpcId,
    /// The method name (the adapter's own namespace).
    pub method: String,
    /// The method parameters; omitted on the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Builds a request with the given id, method, and optional params.
    #[must_use]
    pub fn new(
        id: JsonRpcId,
        method: impl Into<String>,
        params: Option<serde_json::Value>,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 notification: a one-way message with **no `id`**.
///
/// The `Option<id>` discrimination is structural: a notification is exactly a message that omits
/// `id`, so it can never be correlated to (or mistaken for) a request/response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JsonRpcNotification {
    /// The JSON-RPC version, always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The method name (the adapter's own namespace).
    pub method: String,
    /// The notification parameters; omitted on the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcNotification {
    /// Builds a notification with the given method and optional params.
    #[must_use]
    pub fn new(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 error object carried in a failed response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JsonRpcError {
    /// The numeric error code (see [`error_codes`]).
    pub code: i64,
    /// A short human-readable description of the error.
    pub message: String,
    /// Optional structured error data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    /// Builds an error object with a code and message.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// A JSON-RPC 2.0 response: the reply to a request, correlated by matching `id`.
///
/// Exactly one of [`Self::result`] / [`Self::error`] is populated per the spec.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JsonRpcResponse {
    /// The JSON-RPC version, always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The id of the request this response answers.
    pub id: JsonRpcId,
    /// The success payload, when the call succeeded.
    ///
    /// `None` means the `result` key was ABSENT from the frame; a present `"result": null`
    /// decodes to `Some(Value::Null)` (via [`deserialize_present_value`]). The spec makes the
    /// distinction load-bearing: `null` is a legal success payload, while a response carrying
    /// neither `result` nor `error` is a broken frame — the two must not be conflated.
    #[serde(
        default,
        deserialize_with = "deserialize_present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<serde_json::Value>,
    /// The error payload, when the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// Deserializes a field whose KEY PRESENCE matters: serde invokes `deserialize_with` only when
/// the key is present, so this captures the value verbatim — including JSON `null`, which becomes
/// `Some(Value::Null)` instead of collapsing into the absent-key `None` that plain
/// `Option<Value>` would produce. The absent-key case is covered by `#[serde(default)]`.
fn deserialize_present_value<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

impl JsonRpcResponse {
    /// Builds a success response for the given request id.
    #[must_use]
    pub fn success(id: JsonRpcId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Builds an error response for the given request id.
    #[must_use]
    pub fn failure(id: JsonRpcId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A single decoded inbound JSON-RPC message, classified by the `Option<id>` discrimination.
///
/// A frame with `method` + `id` is a [`Self::Request`]; a frame with `method` and no `id` is a
/// [`Self::Notification`]; a frame with `id` and `result`/`error` (no `method`) is a
/// [`Self::Response`]. This is the structural result/event split the whole design relies on: a
/// notification can never carry an id, so it can never be captured as a correlated response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncomingMessage {
    /// A peer-initiated call awaiting a response.
    Request(JsonRpcRequest),
    /// A peer-initiated one-way message (no response).
    Notification(JsonRpcNotification),
    /// A reply to one of our outstanding requests.
    Response(JsonRpcResponse),
}

impl IncomingMessage {
    /// Classifies a decoded JSON value into the correct message kind.
    ///
    /// # Errors
    ///
    /// Returns the decode error when the value matches none of the three JSON-RPC shapes.
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        let has_method = value.get("method").is_some();
        let has_id = value.get("id").is_some_and(|id| !id.is_null());
        if has_method {
            if has_id {
                serde_json::from_value(value).map(Self::Request)
            } else {
                serde_json::from_value(value).map(Self::Notification)
            }
        } else {
            serde_json::from_value(value).map(Self::Response)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        IncomingMessage, JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest,
        JsonRpcResponse, error_codes,
    };

    #[test]
    fn request_serialises_with_id_and_omits_absent_params() -> Result<(), serde_json::Error> {
        let request = JsonRpcRequest::new(JsonRpcId::number(1), "run/execute", None);
        let value = serde_json::to_value(&request)?;
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert_eq!(value["method"], "run/execute");
        assert!(value.get("params").is_none(), "absent params are omitted");
        Ok(())
    }

    #[test]
    fn notification_never_carries_an_id() -> Result<(), serde_json::Error> {
        let notification = JsonRpcNotification::new("event/message", Some(json!({ "text": "hi" })));
        let value = serde_json::to_value(&notification)?;
        assert!(value.get("id").is_none(), "notifications carry no id");
        assert_eq!(value["params"]["text"], "hi");
        Ok(())
    }

    #[test]
    fn classifies_request_notification_and_response() -> Result<(), serde_json::Error> {
        let request = IncomingMessage::from_value(json!({
            "jsonrpc": "2.0", "id": 7, "method": "intervene/inject", "params": {}
        }))?;
        assert!(matches!(request, IncomingMessage::Request(_)));

        let notification = IncomingMessage::from_value(json!({
            "jsonrpc": "2.0", "method": "event/stop", "params": {}
        }))?;
        assert!(matches!(notification, IncomingMessage::Notification(_)));

        let response = IncomingMessage::from_value(json!({
            "jsonrpc": "2.0", "id": 7, "result": { "ok": true }
        }))?;
        assert!(matches!(response, IncomingMessage::Response(_)));
        Ok(())
    }

    #[test]
    fn a_null_id_is_treated_as_a_notification_not_a_request() -> Result<(), serde_json::Error> {
        let message = IncomingMessage::from_value(json!({
            "jsonrpc": "2.0", "id": null, "method": "event/raw", "params": {}
        }))?;
        assert!(
            matches!(message, IncomingMessage::Notification(_)),
            "a null id must not make a method-bearing frame a correlated request"
        );
        Ok(())
    }

    #[test]
    fn error_response_carries_result_none() {
        let response = JsonRpcResponse::failure(
            JsonRpcId::number(3),
            JsonRpcError::new(error_codes::METHOD_NOT_FOUND, "method not found"),
        );
        assert!(response.result.is_none());
        assert!(
            matches!(response.error, Some(error) if error.code == error_codes::METHOD_NOT_FOUND),
            "an error response carries the code"
        );
    }

    #[test]
    fn a_present_null_result_decodes_as_some_null() -> Result<(), serde_json::Error> {
        // `"result": null` is a legal success payload and must stay distinguishable from a frame
        // that carries no `result` key at all.
        let response: JsonRpcResponse = serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": 1, "result": null
        }))?;
        assert_eq!(response.result, Some(serde_json::Value::Null));
        let wire = serde_json::to_value(&response)?;
        assert!(
            wire.as_object()
                .is_some_and(|frame| frame.contains_key("result")),
            "a present null result round-trips with the key on the wire"
        );
        Ok(())
    }

    #[test]
    fn an_absent_result_decodes_as_none() -> Result<(), serde_json::Error> {
        // A frame with neither `result` nor `error` (a broken peer) decodes with `result: None`,
        // never fabricated into a null payload.
        let response: JsonRpcResponse = serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": 1
        }))?;
        assert!(response.result.is_none());
        assert!(response.error.is_none());
        let wire = serde_json::to_value(&response)?;
        assert!(
            wire.as_object()
                .is_some_and(|frame| !frame.contains_key("result")),
            "an absent result stays absent on the wire"
        );
        Ok(())
    }

    #[test]
    fn string_ids_round_trip() -> Result<(), serde_json::Error> {
        let id = JsonRpcId::Text("abc".to_owned());
        let value = serde_json::to_value(&id)?;
        let decoded: JsonRpcId = serde_json::from_value(value)?;
        assert_eq!(decoded, id);
        Ok(())
    }
}
