//! `WorkflowHandle` signal, query, cancel, describe, and subscribe support.

use std::time::Duration;

use aion_core::{Payload, RunId, WorkflowId};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::client::Client;
use crate::error::ClientError;
use crate::ops::WorkflowDescription;
use crate::stream::EventStream;

/// Handle for a concrete workflow run returned by [`Client::start`].
#[derive(Clone)]
pub struct WorkflowHandle {
    client: Client,
    workflow_id: WorkflowId,
    run_id: RunId,
}

impl WorkflowHandle {
    /// Constructs a handle from caller-held workflow and run identifiers.
    ///
    /// The `client` is retained so handle methods can delegate through the same
    /// transport, error mapping, payload machinery, and stream implementation as
    /// top-level [`Client`] operations.
    #[must_use]
    pub fn from_ids(client: Client, workflow_id: WorkflowId, run_id: RunId) -> Self {
        Self {
            client,
            workflow_id,
            run_id,
        }
    }

    /// Returns the workflow identifier bundled in this handle.
    #[must_use]
    pub const fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    /// Returns the run identifier bundled in this handle.
    #[must_use]
    pub const fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// Sends a raw payload signal to this concrete run.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when signal delivery fails.
    pub async fn signal(
        &self,
        name: impl Into<String>,
        payload: Payload,
    ) -> Result<(), ClientError> {
        self.client
            .signal(&self.workflow_id, Some(&self.run_id), name, payload)
            .await
    }

    /// Sends a JSON-typed signal to this concrete run.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidArgument`] when serialization fails, or the
    /// delegated signal error otherwise.
    pub async fn signal_typed<T>(
        &self,
        name: impl Into<String>,
        value: &T,
    ) -> Result<(), ClientError>
    where
        T: Serialize + ?Sized,
    {
        self.client
            .signal_typed(&self.workflow_id, Some(&self.run_id), name, value)
            .await
    }

    /// Queries this concrete run and returns a raw payload result.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the query fails or times out.
    pub async fn query(
        &self,
        name: impl Into<String>,
        args: Payload,
        deadline: Duration,
    ) -> Result<Payload, ClientError> {
        self.client
            .query(&self.workflow_id, Some(&self.run_id), name, args, deadline)
            .await
    }

    /// Queries this concrete run and deserializes the result to `R`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidArgument`] when typed argument serialization
    /// or result decoding fails, or the delegated query error otherwise.
    pub async fn query_typed<A, R>(
        &self,
        name: impl Into<String>,
        args: &A,
        deadline: Duration,
    ) -> Result<R, ClientError>
    where
        A: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.client
            .query_typed(&self.workflow_id, Some(&self.run_id), name, args, deadline)
            .await
    }

    /// Requests cancellation of this concrete run.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the server rejects the cancellation request.
    pub async fn cancel(&self, reason: impl Into<String>) -> Result<(), ClientError> {
        self.client
            .cancel(&self.workflow_id, Some(&self.run_id), reason)
            .await
    }

    /// Describes this concrete run.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the description cannot be fetched or decoded.
    pub async fn describe(&self) -> Result<WorkflowDescription, ClientError> {
        self.client
            .describe(&self.workflow_id, Some(&self.run_id))
            .await
    }

    /// Subscribes to events for this workflow.
    #[must_use]
    pub fn subscribe(&self) -> EventStream {
        self.client.subscribe_workflow(&self.workflow_id)
    }
}

impl PartialEq for WorkflowHandle {
    fn eq(&self, other: &Self) -> bool {
        self.workflow_id == other.workflow_id && self.run_id == other.run_id
    }
}

impl Eq for WorkflowHandle {}

impl std::fmt::Debug for WorkflowHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowHandle")
            .field("workflow_id", &self.workflow_id)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}
