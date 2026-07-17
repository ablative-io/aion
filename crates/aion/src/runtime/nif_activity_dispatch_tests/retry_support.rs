// ---- #197: retry loop at the dispatch seam ------------------------------

/// Scripted dispatcher for the retry-loop tests: pops one outcome per
/// dispatch and records the wire attempt each delivery carried.
struct ScriptedRetryDispatcher {
    outcomes: std::sync::Mutex<std::collections::VecDeque<Result<String, String>>>,
    attempts: std::sync::Mutex<Vec<u32>>,
}

impl ScriptedRetryDispatcher {
    fn new(outcomes: Vec<Result<String, String>>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: std::sync::Mutex::new(outcomes.into_iter().collect()),
            attempts: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn seen_attempts(&self) -> Vec<u32> {
        self.attempts
            .lock()
            .map(|attempts| attempts.clone())
            .unwrap_or_default()
    }
}

impl ActivityDispatcher for ScriptedRetryDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        self.attempts
            .lock()
            .map_err(|_| "attempts lock poisoned".to_owned())?
            .push(request.attempt);
        self.outcomes
            .lock()
            .map_err(|_| "outcomes lock poisoned".to_owned())?
            .pop_front()
            .ok_or_else(|| "terminal:script exhausted — unexpected extra dispatch".to_owned())?
    }
}

/// Store + recorder + request over a seeded `WorkflowStarted` +
/// `ActivityScheduled` + `ActivityStarted(attempt 1)` history — exactly
/// what the dispatch NIF records before spawning the completion task.
struct RetryLoopHarness {
    store: Arc<dyn EventStore>,
    seam: super::RetryRecorderSeam,
    request: ActivityDispatch,
    workflow_id: WorkflowId,
}

impl RetryLoopHarness {
    async fn seeded(config: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(aion_store::InMemoryStore::default());
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(&workflow_id, 1),
                workflow_type: "retrier".to_owned(),
                input: Payload::from_json(&json!({}))?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("b".repeat(64)),
            },
            Event::ActivityScheduled {
                envelope: envelope(&workflow_id, 2),
                activity_id: ActivityId::from_sequence_position(0),
                activity_type: "flaky".to_owned(),
                input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
                task_queue: String::from("default"),
                node: None,
            },
            Event::ActivityStarted {
                envelope: envelope(&workflow_id, 3),
                activity_id: ActivityId::from_sequence_position(0),
                attempt: 1,
            },
        ];
        store
            .append(WriteToken::recorder(), &workflow_id, &events, 0)
            .await?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), 3);
        let request = ActivityDispatch {
            namespace: String::from("default"),
            task_queue: String::from("default"),
            node: None,
            workflow_id: workflow_id.clone(),
            activity_id: ActivityId::from_sequence_position(0),
            name: "flaky".to_owned(),
            input: r#""in""#.to_owned(),
            config: config.to_owned(),
            attempt: super::FIRST_DELIVERY_ATTEMPT,
            labels: std::collections::BTreeMap::new(),
        };
        Ok(Self {
            store,
            seam: super::RetryRecorderSeam {
                recorder: Arc::new(tokio::sync::Mutex::new(recorder)),
                run_id,
            },
            request,
            workflow_id,
        })
    }

    async fn history(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        Ok(self.store.read_history(&self.workflow_id).await?)
    }
}

const FIXED_RETRY_CONFIG: &str =
    r#"{"retry":{"max_attempts":3,"backoff":{"kind":"fixed","delay_ms":2}}}"#;
