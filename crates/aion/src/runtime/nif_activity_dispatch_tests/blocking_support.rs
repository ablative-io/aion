/// Synchronous dispatcher that parks its calling thread on a channel
/// until the test's release task — running on the same Tokio runtime —
/// frees it.
struct GatedDispatcher {
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl ActivityDispatcher for GatedDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let receiver = self
            .release
            .lock()
            .map_err(|_| "release lock poisoned".to_owned())?
            .take()
            .ok_or_else(|| "dispatch invoked more than once".to_owned())?;
        receiver
            .recv()
            .map_err(|error| format!("release channel closed: {error}"))?;
        Ok(request.input)
    }
}

/// The whole single-worker scenario, run on a watchdog-guarded thread:
/// dispatch a gated blocking activity, prove the runtime's only executor
/// thread is still free by releasing the gate from a task spawned on
/// that same runtime, then observe the delivered completion payload.
fn blocking_dispatch_scenario() -> Result<Vec<u8>, String> {
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    tokio_runtime.block_on(async {
        let runtime = Arc::new(
            RuntimeHandle::new(RuntimeConfig::new(Some(1))).map_err(|error| error.to_string())?,
        );
        let pid = runtime
            .spawn_test_process()
            .map_err(|error| error.to_string())?;
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let dispatcher: Arc<dyn ActivityDispatcher> = Arc::new(GatedDispatcher {
            release: std::sync::Mutex::new(Some(release_rx)),
        });
        let workflow_id = WorkflowId::new_v4();
        let recorder = Arc::new(tokio::sync::Mutex::new(Recorder::new(
            workflow_id.clone(),
            Arc::new(aion_store::InMemoryStore::default()),
        )));
        spawn_completion_task(
            &tokio::runtime::Handle::current(),
            Arc::clone(&runtime),
            dispatcher,
            super::RetryRecorderSeam {
                recorder,
                run_id: RunId::new_v4(),
            },
            pid,
            super::correlation_id(0),
            ActivityDispatch {
                namespace: String::from("default"),
                task_queue: String::from("default"),
                node: None,
                workflow_id,
                activity_id: ActivityId::from_sequence_position(0),
                name: "gated".to_owned(),
                input: r#""r0""#.to_owned(),
                config: "{}".to_owned(),
                attempt: super::FIRST_DELIVERY_ATTEMPT,
                labels: std::collections::BTreeMap::new(),
            },
        );
        // The release runs as a task on this same single-threaded
        // runtime, spawned AFTER the completion task: it can only
        // execute if the blocking dispatch is not occupying the
        // executor thread.
        tokio::spawn(async move { release_tx.send(()) })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let mut payload = None;
        for _ in 0_u32..2_000 {
            match runtime.take_activity_result(pid, 0) {
                Ok(Some((delivered, _))) => {
                    payload = Some(delivered.bytes().to_vec());
                    break;
                }
                Ok(None) => {}
                Err(error) => return Err(error.to_string()),
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        runtime.shutdown().map_err(|error| error.to_string())?;
        payload.ok_or_else(|| "activity completion was never delivered".to_owned())
    })
}

/// Regression (closeout rider a): a blocking `ActivityDispatcher` must
/// not wedge a single-threaded engine runtime. Before the
/// `spawn_blocking` routing in `dispatch_async_from_process`, the
/// completion task polled the synchronous dispatch inline on the
/// runtime's only worker thread, so the release task never ran and the
/// dispatch parked forever — stalling every task on the runtime,
/// queries included. The watchdog bounds that wedge to a clean failure
/// instead of a hung suite.
#[test]
fn blocking_dispatcher_completes_on_single_threaded_runtime() -> TestResult {
    let (verdict_tx, verdict_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drop(verdict_tx.send(blocking_dispatch_scenario())));
    let payload = verdict_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .map_err(
            |_| "scenario wedged: the blocking dispatch occupied the only executor thread",
        )??;
    assert_eq!(payload, br#""r0""#.to_vec());
    Ok(())
}
