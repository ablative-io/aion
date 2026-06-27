use std::net::SocketAddr;
use std::time::{Duration, Instant};

use aion_proto::generated::worker_protocol_client::WorkerProtocolClient;
use aion_proto::generated::{self, server_to_worker, worker_to_server};
use tokio_stream::wrappers::ReceiverStream;

use crate::helpers::{FAN_OUT, NAMESPACE, POLL_DEADLINE, TestError, test_error};

const FAN_ACTIVITY_TYPES: [&str; FAN_OUT] = ["fan:0", "fan:1", "fan:2", "fan:3"];

pub struct WorkerSession {
    worker_tx: tokio::sync::mpsc::Sender<generated::WorkerToServer>,
    inbound: tonic::Streaming<generated::ServerToWorker>,
}

impl WorkerSession {
    pub async fn connect(address: SocketAddr) -> Result<Self, TestError> {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            match Self::try_connect(address).await {
                Ok(session) => return Ok(session),
                Err(error) if Instant::now() > deadline => {
                    return Err(test_error(format!(
                        "timed out connecting worker to {address}: {error}"
                    )));
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn try_connect(address: SocketAddr) -> Result<Self, TestError> {
        let mut client = WorkerProtocolClient::connect(format!("http://{address}")).await?;
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<generated::WorkerToServer>(16);
        worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Register(
                    generated::RegisterWorker {
                        namespaces: vec![NAMESPACE.to_owned()],
                        activity_types: FAN_ACTIVITY_TYPES
                            .iter()
                            .map(|activity| (*activity).to_owned())
                            .collect(),
                        task_queue: String::from("default"),
                        node: String::new(),
                    },
                )),
            })
            .await?;
        let mut request = tonic::Request::new(ReceiverStream::new(worker_rx));
        request
            .metadata_mut()
            .insert("x-aion-namespaces", NAMESPACE.parse()?);
        let mut inbound = client.stream_worker(request).await?.into_inner();
        let first = inbound
            .message()
            .await?
            .and_then(|frame| frame.message)
            .ok_or_else(|| test_error("response stream ended before RegisterAck"))?;
        if !matches!(first, server_to_worker::Message::RegisterAck(_)) {
            return Err(test_error(format!(
                "first response frame must be RegisterAck, got {first:?}"
            )));
        }
        Ok(Self { worker_tx, inbound })
    }

    pub async fn next_task(&mut self) -> Result<generated::ActivityTask, TestError> {
        loop {
            let frame = tokio::time::timeout(POLL_DEADLINE, self.inbound.message())
                .await
                .map_err(|_| test_error("timed out waiting for worker activity task"))??;
            match frame.and_then(|message| message.message) {
                Some(server_to_worker::Message::Task(task)) => return Ok(task),
                Some(_) => {}
                None => {
                    return Err(test_error(
                        "worker stream closed before a task was delivered",
                    ));
                }
            }
        }
    }

    pub async fn complete(
        &self,
        task: &generated::ActivityTask,
        result_json: &[u8],
    ) -> Result<(), TestError> {
        self.worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Result(
                    generated::ActivityResult {
                        workflow_id: task.workflow_id.clone(),
                        activity_id: task.activity_id,
                        run_id: task.run_id.clone(),
                        outcome: Some(generated::activity_result::Outcome::Result(
                            generated::Payload {
                                content_type: "application/json".to_owned(),
                                bytes: result_json.to_vec(),
                            },
                        )),
                    },
                )),
            })
            .await?;
        Ok(())
    }
}
