//! Codec conversions between the tonic-generated wire messages and the
//! hand-written `aion-proto` message types the shared handlers consume.
//!
//! Peeled out of `grpc/mod.rs` (per the AO-007 500-code-line split): the
//! service impl and status mapping stay in `mod.rs`; the pure request/response
//! codec functions live here. Every function is `pub(super)` so `mod.rs` calls
//! them unqualified via `use super::convert::*`.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoCreateScheduleRequest, ProtoCreateScheduleResponse,
    ProtoDeleteScheduleResponse, ProtoDescribeScheduleResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoPauseScheduleResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoReopenRequest, ProtoReopenResponse,
    ProtoResumeScheduleResponse, ProtoScheduleIdRequest, ProtoSignalRequest, ProtoSignalResponse,
    ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, ProtoUpdateScheduleRequest,
    ProtoUpdateScheduleResponse, ProtoWireError, generated,
};

pub(super) fn decode_workflow_id(value: generated::WorkflowId) -> aion_proto::ProtoWorkflowId {
    aion_proto::ProtoWorkflowId { uuid: value.uuid }
}

pub(super) fn encode_workflow_id(value: aion_proto::ProtoWorkflowId) -> generated::WorkflowId {
    generated::WorkflowId { uuid: value.uuid }
}

pub(super) fn decode_run_id(value: generated::RunId) -> aion_proto::ProtoRunId {
    aion_proto::ProtoRunId { uuid: value.uuid }
}

pub(super) fn encode_run_id(value: aion_proto::ProtoRunId) -> generated::RunId {
    generated::RunId { uuid: value.uuid }
}

pub(super) fn decode_schedule_id(value: generated::ScheduleId) -> aion_proto::ProtoScheduleId {
    aion_proto::ProtoScheduleId { uuid: value.uuid }
}

pub(super) fn encode_schedule_id(value: aion_proto::ProtoScheduleId) -> generated::ScheduleId {
    generated::ScheduleId { uuid: value.uuid }
}

pub(super) fn decode_payload(value: generated::Payload) -> aion_proto::ProtoPayload {
    aion_proto::ProtoPayload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

pub(super) fn encode_payload(value: aion_proto::ProtoPayload) -> generated::Payload {
    generated::Payload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

pub(super) fn decode_envelope(value: generated::WireEnvelope) -> aion_proto::WireEnvelope {
    aion_proto::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(decode_payload),
    }
}

pub(super) fn encode_envelope(value: aion_proto::WireEnvelope) -> generated::WireEnvelope {
    generated::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(encode_payload),
    }
}

pub(super) fn decode_start_request(
    value: generated::StartWorkflowRequest,
) -> ProtoStartWorkflowRequest {
    ProtoStartWorkflowRequest {
        namespace: value.namespace,
        workflow_type: value.workflow_type,
        input: value.input.map(decode_payload),
        routing_key: value.routing_key,
        task_queue: value.task_queue,
    }
}

pub(super) fn encode_start_response(
    value: ProtoStartWorkflowResponse,
) -> generated::StartWorkflowResponse {
    generated::StartWorkflowResponse {
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
    }
}

pub(super) fn decode_signal_request(value: generated::SignalRequest) -> ProtoSignalRequest {
    ProtoSignalRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        signal_name: value.signal_name,
        payload: value.payload.map(decode_payload),
    }
}

pub(super) fn encode_signal_response(_: ProtoSignalResponse) -> generated::SignalResponse {
    generated::SignalResponse {}
}

pub(super) fn decode_query_request(value: generated::QueryRequest) -> ProtoQueryRequest {
    ProtoQueryRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        query_name: value.query_name,
    }
}

pub(super) fn encode_query_response(value: ProtoQueryResponse) -> generated::QueryResponse {
    generated::QueryResponse {
        outcome: value.outcome.map(encode_query_outcome),
    }
}

pub(super) fn encode_query_outcome(
    value: aion_proto::proto_query_response::Outcome,
) -> generated::query_response::Outcome {
    match value {
        aion_proto::proto_query_response::Outcome::Result(payload) => {
            generated::query_response::Outcome::Result(encode_payload(payload))
        }
        aion_proto::proto_query_response::Outcome::Error(error) => {
            generated::query_response::Outcome::Error(encode_wire_error(error))
        }
    }
}

pub(super) fn encode_wire_error(value: ProtoWireError) -> generated::WireError {
    generated::WireError {
        code: value.code,
        message: value.message,
        error_type: value.error_type,
    }
}

pub(super) fn decode_cancel_request(value: generated::CancelRequest) -> ProtoCancelRequest {
    ProtoCancelRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        reason: value.reason,
    }
}

pub(super) fn encode_cancel_response(_: ProtoCancelResponse) -> generated::CancelResponse {
    generated::CancelResponse {}
}

pub(super) fn decode_reopen_request(value: generated::ReopenRequest) -> ProtoReopenRequest {
    ProtoReopenRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
    }
}

pub(super) fn encode_reopen_response(value: ProtoReopenResponse) -> generated::ReopenResponse {
    generated::ReopenResponse {
        run_id: value.run_id.map(encode_run_id),
        status: value.status,
    }
}

pub(super) fn decode_list_request(
    value: generated::ListWorkflowsRequest,
) -> ProtoListWorkflowsRequest {
    ProtoListWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(decode_envelope),
    }
}

pub(super) fn encode_list_response(
    value: ProtoListWorkflowsResponse,
) -> generated::ListWorkflowsResponse {
    generated::ListWorkflowsResponse {
        summaries: value.summaries.into_iter().map(encode_envelope).collect(),
    }
}

pub(super) fn decode_count_request(
    value: generated::CountWorkflowsRequest,
) -> ProtoCountWorkflowsRequest {
    ProtoCountWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(decode_envelope),
    }
}

pub(super) fn encode_count_response(
    value: ProtoCountWorkflowsResponse,
) -> generated::CountWorkflowsResponse {
    generated::CountWorkflowsResponse { count: value.count }
}

pub(super) fn decode_describe_request(
    value: generated::DescribeWorkflowRequest,
) -> ProtoDescribeWorkflowRequest {
    ProtoDescribeWorkflowRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        include_history: value.include_history,
    }
}

pub(super) fn encode_describe_response(
    value: ProtoDescribeWorkflowResponse,
) -> generated::DescribeWorkflowResponse {
    generated::DescribeWorkflowResponse {
        summary: value.summary.map(encode_envelope),
        history: value.history.into_iter().map(encode_envelope).collect(),
    }
}

pub(super) fn decode_create_schedule_request(
    value: generated::CreateScheduleRequest,
) -> ProtoCreateScheduleRequest {
    ProtoCreateScheduleRequest {
        namespace: value.namespace,
        config: value.config.map(decode_envelope),
    }
}

pub(super) fn encode_create_schedule_response(
    value: ProtoCreateScheduleResponse,
) -> generated::CreateScheduleResponse {
    generated::CreateScheduleResponse {
        schedule_id: value.schedule_id.map(encode_schedule_id),
        state: value.state.map(encode_envelope),
    }
}

pub(super) fn decode_update_schedule_request(
    value: generated::UpdateScheduleRequest,
) -> ProtoUpdateScheduleRequest {
    ProtoUpdateScheduleRequest {
        namespace: value.namespace,
        schedule_id: value.schedule_id.map(decode_schedule_id),
        config: value.config.map(decode_envelope),
    }
}

pub(super) fn encode_update_schedule_response(
    value: ProtoUpdateScheduleResponse,
) -> generated::UpdateScheduleResponse {
    generated::UpdateScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

pub(super) fn decode_schedule_id_request(
    value: generated::ScheduleIdRequest,
) -> ProtoScheduleIdRequest {
    ProtoScheduleIdRequest {
        namespace: value.namespace,
        schedule_id: value.schedule_id.map(decode_schedule_id),
    }
}

pub(super) fn encode_pause_schedule_response(
    value: ProtoPauseScheduleResponse,
) -> generated::PauseScheduleResponse {
    generated::PauseScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

pub(super) fn encode_resume_schedule_response(
    value: ProtoResumeScheduleResponse,
) -> generated::ResumeScheduleResponse {
    generated::ResumeScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

pub(super) fn encode_delete_schedule_response(
    _: ProtoDeleteScheduleResponse,
) -> generated::DeleteScheduleResponse {
    generated::DeleteScheduleResponse {}
}

pub(super) fn decode_list_schedules_request(
    value: generated::ListSchedulesRequest,
) -> ProtoListSchedulesRequest {
    ProtoListSchedulesRequest {
        namespace: value.namespace,
    }
}

pub(super) fn encode_list_schedules_response(
    value: ProtoListSchedulesResponse,
) -> generated::ListSchedulesResponse {
    generated::ListSchedulesResponse {
        schedules: value.schedules.into_iter().map(encode_envelope).collect(),
    }
}

pub(super) fn encode_describe_schedule_response(
    value: ProtoDescribeScheduleResponse,
) -> generated::DescribeScheduleResponse {
    generated::DescribeScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}
