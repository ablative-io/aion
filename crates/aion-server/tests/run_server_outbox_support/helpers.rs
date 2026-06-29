use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aion_core::Event;
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder,
};
use aion_proto::generated;
use aion_store::{OutboxRow, OutboxStatus, ReadableEventStore};
use aion_store_libsql::LibSqlStore;
use serde_json::json;

pub const NAMESPACE: &str = "default";
pub const OUTBOX_MODULE: &str = "aion_outbox_fixture";
pub const FAN_OUT: usize = 4;
pub const POLL_DEADLINE: Duration = Duration::from_secs(20);

const OUTBOX_BEAM: &[u8] = include_bytes!("../fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] = include_bytes!("../fixtures/aion_outbox_fixture.erl");
pub type TestError = Box<dyn std::error::Error + Send + Sync>;

pub fn test_error(message: impl Into<String>) -> TestError {
    std::io::Error::other(message.into()).into()
}

pub fn unique_temp_dir(name: &str) -> Result<tempfile::TempDir, TestError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(tempfile::Builder::new()
        .prefix(&format!(
            "aion-run-server-outbox-{name}-{pid}-{nanos}-{unique}-"
        ))
        .tempdir()?)
}

pub fn write_package_archive(dir: &Path) -> Result<PathBuf, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)])?;
    let manifest = Manifest {
        entry_module: OUTBOX_MODULE.to_owned(),
        entry_function: "collect_four".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
            .write_to_bytes()?;
    let path = dir.join("collect_four.aion");
    std::fs::write(&path, archive)?;
    Ok(path)
}

pub struct ServerProcess {
    child: Child,
}

impl ServerProcess {
    fn spawn(config_path: &Path) -> Result<Self, TestError> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--exact")
            .arg("run_server_child_process")
            .arg("--nocapture");
        for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("AION_")) {
            command.env_remove(name);
        }
        command
            .env("AION_RUN_SERVER_CHILD", "1")
            .env("AION_RUN_SERVER_CONFIG", config_path);
        Ok(Self {
            child: command.spawn()?,
        })
    }

    async fn wait_ready(&mut self, http: SocketAddr) -> Result<(), TestError> {
        let client = reqwest::Client::new();
        let url = format!("http://{http}/health/ready");
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(test_error(format!(
                    "run_server child exited before readiness: {status}"
                )));
            }
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(test_error("timed out waiting for run_server readiness"));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn stop(mut self) -> Result<(), TestError> {
        self.kill_and_wait()
    }

    fn kill_and_wait(&mut self) -> Result<(), TestError> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        let status = self.child.wait()?;
        std::hint::black_box(status);
        Ok(())
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub async fn start_over_http(
    address: SocketAddr,
) -> Result<(aion_core::WorkflowId, aion_core::RunId), TestError> {
    let client = reqwest::Client::new();
    let url = format!("http://{address}/workflows/start");
    let mut last_error = String::new();
    for attempt in 0..5 {
        match client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-aion-subject", "ci")
            .header("x-aion-namespaces", NAMESPACE)
            .json(&json!({
                "namespace": NAMESPACE,
                "workflow_type": OUTBOX_MODULE,
                "input": { "fixture": "input" },
            }))
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let bytes = response.bytes().await?;
                if status.is_success() {
                    return workflow_ids_from_start_body(&bytes);
                }
                last_error = format!("HTTP {status}: {}", String::from_utf8_lossy(&bytes));
                if !status.is_server_error() {
                    break;
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
    }
    Err(test_error(format!(
        "workflow start over HTTP failed: {last_error}"
    )))
}

fn workflow_ids_from_start_body(
    bytes: &[u8],
) -> Result<(aion_core::WorkflowId, aion_core::RunId), TestError> {
    // Clean wire contract: start response exposes plain UUID strings.
    let body: serde_json::Value = serde_json::from_slice(bytes)?;
    let workflow_id = body["workflow_id"]
        .as_str()
        .ok_or_else(|| test_error("start response missing workflow id"))?
        .parse::<uuid::Uuid>()?;
    let run_id = body["run_id"]
        .as_str()
        .ok_or_else(|| test_error("start response missing run id"))?
        .parse::<uuid::Uuid>()?;
    Ok((
        aion_core::WorkflowId::new(workflow_id),
        aion_core::RunId::new(run_id),
    ))
}

pub fn count_kind(history: &[Event], matcher: impl Fn(&Event) -> bool) -> usize {
    history.iter().filter(|event| matcher(event)).count()
}

pub fn count_completed(history: &[Event]) -> usize {
    count_kind(history, |event| {
        matches!(event, Event::ActivityCompleted { .. })
    })
}

pub fn count_completed_for(history: &[Event], ordinal: u64) -> usize {
    count_kind(history, |event| match event {
        Event::ActivityCompleted { activity_id, .. } => activity_id.sequence_position() == ordinal,
        _ => false,
    })
}

pub async fn wait_for_history<F>(
    store: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, TestError>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if Instant::now() > deadline {
            return Err(test_error(format!(
                "timed out waiting for {description}: {history:#?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn wait_for_rows<F>(
    store: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
    ordinals: &[u64],
    description: &str,
    predicate: F,
) -> Result<Vec<OutboxStatus>, TestError>
where
    F: Fn(&[OutboxStatus]) -> bool,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let statuses = row_statuses(store, workflow_id, ordinals).await?;
        if predicate(&statuses) {
            return Ok(statuses);
        }
        if Instant::now() > deadline {
            return Err(test_error(format!(
                "timed out waiting for {description}: {statuses:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn row_statuses(
    store: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
    ordinals: &[u64],
) -> Result<Vec<OutboxStatus>, TestError> {
    let mut statuses = Vec::with_capacity(ordinals.len());
    for ordinal in ordinals {
        let key = OutboxRow::dispatch_key_for(workflow_id, *ordinal);
        let state = store
            .outbox_row_state(&key)
            .await?
            .ok_or_else(|| test_error(format!("no outbox row for ordinal {ordinal}")))?;
        statuses.push(state.status);
    }
    Ok(statuses)
}

pub fn worker_result(ordinal: u64) -> String {
    format!("\"worker-{ordinal}\"")
}

pub fn task_ordinal(task: &generated::ActivityTask) -> Result<u64, TestError> {
    task.activity_id
        .as_ref()
        .map(|id| id.sequence_position)
        .ok_or_else(|| test_error("pushed task missing activity id"))
}

pub fn assert_task_set(
    tasks: &[generated::ActivityTask],
    expected: &[u64],
) -> Result<(), TestError> {
    let mut ordinals = tasks
        .iter()
        .map(task_ordinal)
        .collect::<Result<Vec<_>, _>>()?;
    ordinals.sort_unstable();
    assert_eq!(ordinals, expected);
    Ok(())
}

pub async fn assert_fan_out_settled(
    reader: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
) -> Result<Vec<Event>, TestError> {
    let history = wait_for_history(reader, workflow_id, "fan-out settled", |events| {
        count_completed(events) == FAN_OUT
            && count_kind(events, |event| {
                matches!(event, Event::WorkflowCompleted { .. })
            }) == 1
    })
    .await?;
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(count_completed_for(&history, ordinal), 1);
    }
    wait_for_rows(
        reader,
        workflow_id,
        &[0, 1, 2, 3],
        "all rows done",
        |statuses| statuses.iter().all(|status| *status == OutboxStatus::Done),
    )
    .await?;
    assert_collect_result(&history)?;
    Ok(history)
}

fn assert_collect_result(history: &[Event]) -> Result<(), TestError> {
    let result = history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .ok_or_else(|| test_error("no WorkflowCompleted result payload"))?;
    let value: serde_json::Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ])
    );
    Ok(())
}

pub async fn run_server_harness(
    dir: &Path,
    db_path: &Path,
    package_path: &Path,
) -> Result<(ServerProcess, SocketAddr, SocketAddr), TestError> {
    run_server_harness_with_reconciliation(dir, db_path, package_path, None).await
}

pub async fn run_server_harness_with_reconciliation(
    dir: &Path,
    db_path: &Path,
    package_path: &Path,
    reconciliation: Option<(u64, u64)>,
) -> Result<(ServerProcess, SocketAddr, SocketAddr), TestError> {
    let http = reserve_loopback_addr()?;
    let grpc = reserve_loopback_addr()?;
    let config = write_server_config(dir, db_path, package_path, http, grpc, reconciliation)?;
    let mut server = ServerProcess::spawn(&config)?;
    server.wait_ready(http).await?;
    Ok((server, http, grpc))
}

fn reserve_loopback_addr() -> Result<SocketAddr, TestError> {
    let listener = StdTcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(address)
}

fn write_server_config(
    dir: &Path,
    db_path: &Path,
    package_path: &Path,
    http: SocketAddr,
    grpc: SocketAddr,
    reconciliation: Option<(u64, u64)>,
) -> Result<PathBuf, TestError> {
    let reconciliation = reconciliation.map_or_else(String::new, |(interval, stale_after)| {
        format!("reconcile_interval_ms = {interval}\nreconcile_stale_after_ms = {stale_after}\n")
    });
    let config = format!(
        r#"workflow_packages = [{package}]

[server]
listen_address = "{http}"
grpc_address = "{grpc}"

[store]
backend = "libsql"
url = {db}

[runtime]
scheduler_threads = 1
query_timeout_ms = 10000

[drain]
timeout_seconds = 30

[namespaces]
default = "{NAMESPACE}"

[metrics]
enabled = false

[websocket]
outbound_buffer_bound = 32
event_broadcast_capacity = 64
cluster_broadcast_capacity = 64

[outbox]
enabled = true
# The default build compiles liminal-transport, which makes the outbox transport
# default to liminal; this test exercises the gRPC connected-worker path, so it
# selects grpc explicitly (otherwise an enabled outbox would require a liminal
# listen address).
transport = "grpc"
poll_interval_ms = 1000
batch_size = 16
max_attempts = 5
backoff_base_ms = 50
backoff_multiplier = 2
backoff_max_ms = 1000
{reconciliation}
"#,
        package = toml_string(&package_path.display().to_string())?,
        db = toml_string(&db_path.display().to_string())?,
    );
    let path = dir.join(format!("server-{}.toml", grpc.port()));
    std::fs::write(&path, config)?;
    Ok(path)
}

fn toml_string(value: &str) -> Result<String, TestError> {
    serde_json::to_string(value).map_err(Into::into)
}
