//! Conversions between the hand-written `aion-proto` types and the
//! tonic-generated message types used on the gRPC wire.

pub(crate) fn encode_workflow_id(
    value: aion_proto::ProtoWorkflowId,
) -> aion_proto::generated::WorkflowId {
    aion_proto::generated::WorkflowId { uuid: value.uuid }
}

pub(crate) fn decode_workflow_id(
    value: aion_proto::generated::WorkflowId,
) -> aion_proto::ProtoWorkflowId {
    aion_proto::ProtoWorkflowId { uuid: value.uuid }
}

pub(crate) fn encode_run_id(value: aion_proto::ProtoRunId) -> aion_proto::generated::RunId {
    aion_proto::generated::RunId { uuid: value.uuid }
}

pub(crate) fn decode_run_id(value: aion_proto::generated::RunId) -> aion_proto::ProtoRunId {
    aion_proto::ProtoRunId { uuid: value.uuid }
}

pub(crate) fn encode_payload(value: aion_proto::ProtoPayload) -> aion_proto::generated::Payload {
    aion_proto::generated::Payload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

pub(crate) fn decode_payload(value: aion_proto::generated::Payload) -> aion_proto::ProtoPayload {
    aion_proto::ProtoPayload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

pub(crate) fn decode_wire_error(
    value: aion_proto::generated::WireError,
) -> aion_proto::ProtoWireError {
    aion_proto::ProtoWireError {
        code: value.code,
        message: value.message,
        error_type: value.error_type,
    }
}

pub(crate) fn encode_envelope(
    value: aion_proto::WireEnvelope,
) -> aion_proto::generated::WireEnvelope {
    aion_proto::generated::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(encode_payload),
    }
}

pub(crate) fn decode_envelope(
    value: aion_proto::generated::WireEnvelope,
) -> aion_proto::WireEnvelope {
    aion_proto::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(decode_payload),
    }
}

pub(crate) fn encode_start_request(
    value: aion_proto::ProtoStartWorkflowRequest,
) -> aion_proto::generated::StartWorkflowRequest {
    aion_proto::generated::StartWorkflowRequest {
        namespace: value.namespace,
        workflow_type: value.workflow_type,
        input: value.input.map(encode_payload),
        routing_key: value.routing_key,
    }
}

pub(crate) fn decode_start_response(
    value: aion_proto::generated::StartWorkflowResponse,
) -> aion_proto::ProtoStartWorkflowResponse {
    aion_proto::ProtoStartWorkflowResponse {
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
    }
}

pub(crate) fn encode_signal_request(
    value: aion_proto::ProtoSignalRequest,
) -> aion_proto::generated::SignalRequest {
    aion_proto::generated::SignalRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        signal_name: value.signal_name,
        payload: value.payload.map(encode_payload),
    }
}

pub(crate) fn decode_signal_response(
    _: aion_proto::generated::SignalResponse,
) -> aion_proto::ProtoSignalResponse {
    aion_proto::ProtoSignalResponse {}
}

pub(crate) fn encode_query_request(
    value: aion_proto::ProtoQueryRequest,
) -> aion_proto::generated::QueryRequest {
    aion_proto::generated::QueryRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        query_name: value.query_name,
    }
}

pub(crate) fn decode_query_response(
    value: aion_proto::generated::QueryResponse,
) -> aion_proto::ProtoQueryResponse {
    aion_proto::ProtoQueryResponse {
        outcome: value.outcome.map(decode_query_outcome),
    }
}

pub(crate) fn decode_query_outcome(
    value: aion_proto::generated::query_response::Outcome,
) -> aion_proto::proto_query_response::Outcome {
    match value {
        aion_proto::generated::query_response::Outcome::Result(payload) => {
            aion_proto::proto_query_response::Outcome::Result(decode_payload(payload))
        }
        aion_proto::generated::query_response::Outcome::Error(error) => {
            aion_proto::proto_query_response::Outcome::Error(decode_wire_error(error))
        }
    }
}

pub(crate) fn encode_cancel_request(
    value: aion_proto::ProtoCancelRequest,
) -> aion_proto::generated::CancelRequest {
    aion_proto::generated::CancelRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        reason: value.reason,
    }
}

pub(crate) fn decode_cancel_response(
    _: aion_proto::generated::CancelResponse,
) -> aion_proto::ProtoCancelResponse {
    aion_proto::ProtoCancelResponse {}
}

pub(crate) fn encode_list_request(
    value: aion_proto::ProtoListWorkflowsRequest,
) -> aion_proto::generated::ListWorkflowsRequest {
    aion_proto::generated::ListWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(encode_envelope),
    }
}

pub(crate) fn decode_list_response(
    value: aion_proto::generated::ListWorkflowsResponse,
) -> aion_proto::ProtoListWorkflowsResponse {
    aion_proto::ProtoListWorkflowsResponse {
        summaries: value.summaries.into_iter().map(decode_envelope).collect(),
    }
}

pub(crate) fn encode_describe_request(
    value: aion_proto::ProtoDescribeWorkflowRequest,
) -> aion_proto::generated::DescribeWorkflowRequest {
    aion_proto::generated::DescribeWorkflowRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        include_history: value.include_history,
    }
}

pub(crate) fn decode_describe_response(
    value: aion_proto::generated::DescribeWorkflowResponse,
) -> aion_proto::ProtoDescribeWorkflowResponse {
    aion_proto::ProtoDescribeWorkflowResponse {
        summary: value.summary.map(decode_envelope),
        history: value.history.into_iter().map(decode_envelope).collect(),
    }
}
