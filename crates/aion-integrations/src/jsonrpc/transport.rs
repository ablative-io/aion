//! Newline-delimited JSON-RPC 2.0 framing over an async duplex, with a single serializing
//! writer and request-id correlation.
//!
//! The one net-new piece over the in-tree prior art (§9.4): because this channel is
//! **bidirectional**, responses and outbound notifications could interleave and corrupt a frame.
//! [`JsonRpcConnection`] serialises all writes through a single [`tokio::sync::Mutex`]-guarded
//! writer so every frame is emitted atomically, and allocates monotonic request ids from a second
//! guarded counter so an adapter can correlate a response to the request it sent.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::sync::Mutex;

use super::envelope::{
    IncomingMessage, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use crate::error::HarnessError;

/// A framed JSON-RPC 2.0 connection over one async read half and one async write half.
///
/// Generic over any [`AsyncRead`] + [`AsyncWrite`] pair — a child's stdout/stdin, an in-memory
/// duplex in tests, or a socket. The read and write halves are independent, so a caller may read
/// inbound frames on one task while another task writes outbound frames concurrently: writes are
/// serialised internally, and the id allocator is shared, so both are safe to use behind a shared
/// reference (e.g. an [`std::sync::Arc`]).
pub struct JsonRpcConnection<R, W> {
    reader: Mutex<Lines<BufReader<R>>>,
    writer: Mutex<W>,
    next_id: AtomicU64,
}

impl<R, W> JsonRpcConnection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Wraps a read half and a write half into a framed connection.
    #[must_use]
    pub fn new(read_half: R, write_half: W) -> Self {
        Self {
            reader: Mutex::new(BufReader::new(read_half).lines()),
            writer: Mutex::new(write_half),
            next_id: AtomicU64::new(1),
        }
    }

    /// Allocates the next monotonic request id.
    ///
    /// Ids are process-local to this connection and used only to correlate a response to the
    /// request that produced it, so a plain monotonic counter is correct here.
    #[must_use]
    pub fn next_request_id(&self) -> JsonRpcId {
        JsonRpcId::number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Writes one JSON-RPC request as a newline-delimited frame.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Protocol`] if the request cannot be serialised, or
    /// [`HarnessError::Transport`] if the underlying write fails.
    pub async fn send_request(&self, request: &JsonRpcRequest) -> Result<(), HarnessError> {
        self.write_frame(request).await
    }

    /// Writes one JSON-RPC notification as a newline-delimited frame.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Protocol`] if the notification cannot be serialised, or
    /// [`HarnessError::Transport`] if the underlying write fails.
    pub async fn send_notification(
        &self,
        notification: &JsonRpcNotification,
    ) -> Result<(), HarnessError> {
        self.write_frame(notification).await
    }

    /// Writes one JSON-RPC response as a newline-delimited frame.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Protocol`] if the response cannot be serialised, or
    /// [`HarnessError::Transport`] if the underlying write fails.
    pub async fn send_response(&self, response: &JsonRpcResponse) -> Result<(), HarnessError> {
        self.write_frame(response).await
    }

    /// Reads the next inbound frame, classifying it into an [`IncomingMessage`].
    ///
    /// Returns `Ok(None)` at end of stream (the peer closed its write half). Blank lines are
    /// skipped, matching the newline-delimited framing convention.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Transport`] on an I/O failure and [`HarnessError::Protocol`] on a
    /// frame that is not a valid JSON-RPC message.
    pub async fn recv(&self) -> Result<Option<IncomingMessage>, HarnessError> {
        let mut reader = self.reader.lock().await;
        loop {
            let line = reader
                .next_line()
                .await
                .map_err(|source| HarnessError::transport(format!("read failed: {source}")))?;
            let Some(line) = line else {
                return Ok(None);
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|source| {
                HarnessError::protocol(format!("invalid JSON frame: {source}"))
            })?;
            let message = IncomingMessage::from_value(value).map_err(|source| {
                HarnessError::protocol(format!("frame is not a JSON-RPC message: {source}"))
            })?;
            return Ok(Some(message));
        }
    }

    /// Serialises `frame` to a single line and writes it atomically through the guarded writer.
    async fn write_frame<T: Serialize>(&self, frame: &T) -> Result<(), HarnessError> {
        let mut encoded = serde_json::to_vec(frame).map_err(|source| {
            HarnessError::protocol(format!("frame cannot be encoded: {source}"))
        })?;
        encoded.push(b'\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(&encoded)
            .await
            .map_err(|source| HarnessError::transport(format!("write failed: {source}")))?;
        writer
            .flush()
            .await
            .map_err(|source| HarnessError::transport(format!("flush failed: {source}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::super::envelope::{IncomingMessage, JsonRpcNotification, JsonRpcRequest};
    use super::JsonRpcConnection;
    use crate::error::HarnessError;

    #[tokio::test]
    async fn request_and_notification_round_trip_over_a_duplex() -> Result<(), HarnessError> {
        // A loopback pair: host writes into `host`, child reads from `child`, and vice versa.
        let (host_io, child_io) = duplex(4096);
        let (host_read, host_write) = tokio::io::split(host_io);
        let (child_read, child_write) = tokio::io::split(child_io);
        let host = JsonRpcConnection::new(host_read, host_write);
        let child = JsonRpcConnection::new(child_read, child_write);

        let id = host.next_request_id();
        let request = JsonRpcRequest::new(id.clone(), "run/execute", None);
        host.send_request(&request).await?;

        let received = child
            .recv()
            .await?
            .ok_or_else(|| HarnessError::protocol("expected a frame, got end-of-stream"))?;
        assert!(
            matches!(&received, IncomingMessage::Request(got) if got.id == id && got.method == "run/execute"),
            "expected the id-matched run/execute request, got {received:?}"
        );

        // The child replies with a notification (no id): it must classify as a notification.
        let notification = JsonRpcNotification::new("event/stop", None);
        child.send_notification(&notification).await?;
        let echoed = host
            .recv()
            .await?
            .ok_or_else(|| HarnessError::protocol("expected a frame, got end-of-stream"))?;
        assert!(
            matches!(echoed, IncomingMessage::Notification(_)),
            "a frame with no id must classify as a notification"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ids_are_monotonic() {
        let (a, _b) = duplex(64);
        let (read, write) = tokio::io::split(a);
        let connection = JsonRpcConnection::new(read, write);
        let first = connection.next_request_id();
        let second = connection.next_request_id();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn recv_returns_none_at_end_of_stream() -> Result<(), HarnessError> {
        let (host_io, child_io) = duplex(64);
        let (host_read, host_write) = tokio::io::split(host_io);
        let host = JsonRpcConnection::new(host_read, host_write);
        // Dropping the child's io closes the write half the host reads from.
        drop(child_io);
        let end = host.recv().await?;
        assert!(end.is_none(), "closed stream yields None");
        Ok(())
    }
}
