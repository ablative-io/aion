//! Single error-rendering path shared by every aion-cli subcommand.
//!
//! Contract: every operational failure prints to stderr and the process
//! exits with code 1. When the failure carries the client taxonomy
//! ([`ClientError`] anywhere in the anyhow chain) the first line is
//!
//! ```text
//! error[<class>]: <operation context>: <server detail message>
//! ```
//!
//! where `<class>` is the stable taxonomy class aligned with the wire error
//! codes (`not_found`, `namespace_denied`, `invalid_input`, `backend`,
//! `query_failed`, `query_timeout`, `unknown_query`, `not_running`, ...).
//! The structured wire `error_type` and an actionable hint follow on their
//! own indented lines when available. Failures without a taxonomy class
//! render the full anyhow cause chain on one `error:` line, so no underlying
//! detail is ever masked by a context message.

use aion_client::ClientError;
use aion_proto::{WireError, WireErrorCode};

/// Renders any CLI failure into the stderr contract described in the module
/// docs. Deploy subcommands carry the raw typed [`WireError`] (their wire
/// codes — `deploy_denied`, `version_pinned` — are operator surface and
/// deliberately outside the caller SDK taxonomy).
pub(crate) fn render_error(error: &anyhow::Error) -> String {
    if let Some(client_error) = find_client_error(error) {
        return render_client_error(error, client_error);
    }
    if let Some(wire) = find_wire_error(error) {
        return render_wire_error(error, wire);
    }
    format!("error: {}", joined_chain(error))
}

/// Finds a raw wire error (deploy path) anywhere in the anyhow chain.
fn find_wire_error(error: &anyhow::Error) -> Option<&WireError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WireError>())
}

fn render_wire_error(error: &anyhow::Error, wire: &WireError) -> String {
    let mut rendered = format!("error[{}]: ", wire.code);
    for layer in error
        .chain()
        .take_while(|cause| cause.downcast_ref::<WireError>().is_none())
    {
        rendered.push_str(&layer.to_string());
        rendered.push_str(": ");
    }
    if wire.message.is_empty() {
        rendered.push_str("(the server supplied no detail message)");
    } else {
        rendered.push_str(&wire.message);
    }
    if let Some(error_type) = &wire.error_type {
        rendered.push_str("\n  server error type: ");
        rendered.push_str(error_type);
    }
    if let Some(hint) = wire_hint(wire.code) {
        rendered.push_str("\n  hint: ");
        rendered.push_str(hint);
    }
    rendered
}

/// Actionable hints for the deploy wire codes the CLI can surface.
const fn wire_hint(code: WireErrorCode) -> Option<&'static str> {
    match code {
        WireErrorCode::DeployDenied => Some(
            "this caller holds no deploy grant; pass --token (or AION_TOKEN) with a \
             token whose deploy claim is true, or in development mode check the \
             server's denial detail above",
        ),
        WireErrorCode::VersionPinned => Some(
            "the version is route-active or pinned by live state; `aion versions` \
             shows what is routed — route another version first, or wait for the \
             pinning runs to finish",
        ),
        WireErrorCode::NotFound => Some(
            "the (workflow-type, content-hash) pair is not loaded; `aion versions` \
             lists every loaded version",
        ),
        _ => None,
    }
}

/// Finds the taxonomy error anywhere in the anyhow context chain.
fn find_client_error(error: &anyhow::Error) -> Option<&ClientError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ClientError>())
}

fn render_client_error(error: &anyhow::Error, client_error: &ClientError) -> String {
    let mut rendered = format!("error[{}]: ", client_error.class());
    // Context layers above the ClientError (operation labels such as
    // "failed to query workflow") keep their place in the message; the
    // ClientError itself contributes its detail, not its Display, so the
    // class is never printed twice.
    for layer in error
        .chain()
        .take_while(|cause| cause.downcast_ref::<ClientError>().is_none())
    {
        rendered.push_str(&layer.to_string());
        rendered.push_str(": ");
    }
    let detail = client_error.detail();
    if detail.message.is_empty() {
        rendered.push_str("(the server supplied no detail message)");
    } else {
        rendered.push_str(&detail.message);
    }
    if let Some(error_type) = &detail.error_type {
        rendered.push_str("\n  server error type: ");
        rendered.push_str(error_type);
    }
    if let Some(hint) = hint(client_error) {
        rendered.push_str("\n  hint: ");
        rendered.push_str(hint);
    }
    rendered
}

/// Joins every layer of an anyhow chain, so `.context(...)` labels never
/// mask the root cause.
fn joined_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// Actionable per-class hints. Classes whose detail message is already
/// self-sufficient (`already_exists`, `cancelled`, `invalid_input`,
/// `backend`) carry none.
const fn hint(error: &ClientError) -> Option<&'static str> {
    match error {
        ClientError::NotFound { .. } => Some(
            "verify the workflow id, --run-id, and --namespace; workflows in \
             other namespaces are reported as not found",
        ),
        ClientError::QueryFailed { .. } => Some(
            "the workflow's query handler ran and reported this failure; inspect \
             the handler, or the run with `aion describe <workflow-id>`",
        ),
        ClientError::QueryTimeout { .. } => Some(
            "the query missed its deadline; the workflow may be busy or stalled \
             — retry, or inspect the run with `aion describe <workflow-id>`",
        ),
        ClientError::UnknownQuery { .. } => Some(
            "the workflow does not register a query with this name; check the \
             query name against the workflow's query handlers",
        ),
        ClientError::NotRunning { .. } => Some(
            "the target run is no longer running; `aion list --status \
             running` shows runs that can still serve queries and signals",
        ),
        ClientError::NamespaceDenied { .. } => Some(
            "this caller has no grant for the requested namespace; pass a \
             --namespace the caller is authorized for",
        ),
        ClientError::Unauthenticated { .. } => {
            Some("the server rejected the caller's credentials; check the auth token and --subject")
        }
        ClientError::Unavailable { .. } => {
            Some("cannot reach the server; check --endpoint and that aion-server is running")
        }
        ClientError::InvalidState { .. } => Some(
            "the target run is not in a state this operation accepts (e.g. reopen \
             requires a terminal Failed or Cancelled run); inspect it with `aion \
             describe <workflow-id>`",
        ),
        ClientError::AlreadyExists { .. }
        | ClientError::Cancelled { .. }
        | ClientError::InvalidArgument { .. }
        | ClientError::Server { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use aion_client::{ClientError, ErrorDetail};

    use super::render_error;

    fn rendered(client_error: ClientError, context: &'static str) -> String {
        let error = anyhow::Error::new(client_error).context(context);
        render_error(&error)
    }

    #[test]
    fn each_query_wire_code_renders_a_distinct_class_detail_and_hint() {
        let cases = [
            (
                ClientError::query_failed("handler raised: cart is empty"),
                "error[query_failed]: failed to query workflow: handler raised: cart is empty",
                "query handler ran and reported",
            ),
            (
                ClientError::query_timeout("query window of 5s elapsed"),
                "error[query_timeout]: failed to query workflow: query window of 5s elapsed",
                "missed its deadline",
            ),
            (
                ClientError::unknown_query("no query named 'stat' is registered"),
                "error[unknown_query]: failed to query workflow: no query named 'stat' is \
                 registered",
                "does not register a query with this name",
            ),
            (
                ClientError::not_running("run already reached Completed"),
                "error[not_running]: failed to query workflow: run already reached Completed",
                "no longer running",
            ),
        ];
        for (client_error, first_line, hint_fragment) in cases {
            let output = rendered(client_error, "failed to query workflow");
            let mut lines = output.lines();
            assert_eq!(lines.next(), Some(first_line));
            let hint = lines.next().unwrap_or_default();
            assert!(
                hint.starts_with("  hint: ") && hint.contains(hint_fragment),
                "hint line for {first_line:?} was {hint:?}"
            );
        }
    }

    #[test]
    fn structured_error_type_gets_its_own_line() {
        let output = rendered(
            ClientError::server(ErrorDetail::with_type("store unavailable", "Durability")),
            "failed to start workflow",
        );
        assert_eq!(
            output,
            "error[backend]: failed to start workflow: store unavailable\n  server error type: \
             Durability"
        );
    }

    #[test]
    fn unavailable_renders_transport_chain_and_endpoint_hint() {
        let output = rendered(
            ClientError::unavailable("transport error: tcp connect error: connection refused"),
            "failed to connect to Aion server",
        );
        assert_eq!(
            output,
            "error[unavailable]: failed to connect to Aion server: transport error: tcp connect \
             error: connection refused\n  hint: cannot reach the server; check --endpoint and \
             that aion-server is running"
        );
    }

    #[test]
    fn namespace_denied_and_not_found_render_their_classes() {
        let denied = rendered(
            ClientError::namespace_denied("namespace tenant-b is not granted to this caller"),
            "failed to list workflows",
        );
        assert!(
            denied.starts_with(
                "error[namespace_denied]: failed to list workflows: namespace tenant-b is not \
                 granted to this caller"
            ),
            "got {denied:?}"
        );

        let not_found = rendered(
            ClientError::not_found(ErrorDetail::with_type(
                "workflow was not found",
                "WorkflowNotFound",
            )),
            "failed to describe workflow",
        );
        assert!(
            not_found
                .contains("error[not_found]: failed to describe workflow: workflow was not found")
                && not_found.contains("  server error type: WorkflowNotFound"),
            "got {not_found:?}"
        );
    }

    #[test]
    fn invalid_input_and_backend_render_without_a_hint() {
        for client_error in [
            ClientError::invalid_argument("resume_from_seq must be >= 1"),
            ClientError::server("query response outcome is missing"),
            ClientError::already_exists("idempotency key conflict"),
            ClientError::cancelled("call cancelled"),
        ] {
            let output = rendered(client_error, "operation failed");
            assert!(
                !output.contains("\n  hint: "),
                "detail is self-sufficient, got {output:?}"
            );
        }
    }

    #[test]
    fn empty_server_detail_is_stated_not_hidden() {
        let output = rendered(ClientError::cancelled(""), "failed to cancel workflow");
        assert_eq!(
            output,
            "error[cancelled]: failed to cancel workflow: (the server supplied no detail message)"
        );
    }

    #[test]
    fn non_client_errors_render_the_full_anyhow_chain() {
        let root = std::io::Error::new(std::io::ErrorKind::NotFound, "workflow.toml is missing");
        let error = anyhow::Error::new(root).context("failed to package workflow project");
        assert_eq!(
            render_error(&error),
            "error: failed to package workflow project: workflow.toml is missing"
        );
    }

    #[test]
    fn nested_context_layers_all_appear_before_the_detail() {
        let error = anyhow::Error::new(ClientError::not_running("run is terminal"))
            .context("failed to signal workflow")
            .context("signal operation aborted");
        let output = render_error(&error);
        assert!(
            output.starts_with(
                "error[not_running]: signal operation aborted: failed to signal workflow: run is \
                 terminal"
            ),
            "got {output:?}"
        );
    }
}
