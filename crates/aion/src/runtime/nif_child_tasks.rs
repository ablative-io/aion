//! Dedicated runtime and lifecycle registry for engine-side child tasks:
//! child-terminal watcher tasks and post-record spawn-recovery tasks.
//!
//! These tasks must not outlive the engine epoch: a watcher still running
//! after shutdown could double-write a parent history that a successor
//! engine instance over the same store also records into. Tokio's `abort`
//! alone does not guarantee that — an aborted task finishes its in-flight
//! poll (which can be a recorder append) — so the epoch close must *await*
//! every aborted task. Awaiting a task parked on the host's runtime from a
//! synchronous `Engine::shutdown` would deadlock a current-thread host
//! runtime (the blocked thread is the one that drives the tasks), so the
//! tasks run on an engine-owned runtime with its own worker thread: shutdown
//! can block on a channel while that worker drives every abort to
//! completion, regardless of the host runtime flavor.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use aion_core::WorkflowId;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::task::JoinHandle;

use crate::EngineError;

/// Engine-owned child-task executor and task-handle registry.
///
/// Arming is gated: once [`ChildTaskRuntime::shutdown`] begins, no new task
/// can be armed, every retained handle is aborted *and awaited*, and the
/// owned runtime is released — only then is the epoch considered closed.
/// Dropping the registry without an explicit shutdown is backstopped by
/// [`Drop`], which aborts everything and releases the runtime without
/// blocking (safe in any context).
pub(crate) struct ChildTaskRuntime {
    /// Owned executor; `None` once shut down.
    ///
    /// One dedicated worker thread: the tasks are pure async (store reads,
    /// recorder appends, backoff sleeps, doorbell awaits), so a single
    /// worker drives any number of them; what matters is that it is *not* a
    /// host-runtime thread, so shutdown can block on it safely.
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    /// Armed child-terminal watcher tasks keyed by `(parent pid, child id)`.
    ///
    /// beamr never reuses pids within a scheduler, so a removed key can
    /// never collide with a later process.
    watches: DashMap<(u64, WorkflowId), JoinHandle<()>>,
    /// Spawn-recovery tasks keyed by the recorded child workflow id.
    spawn_retries: DashMap<WorkflowId, JoinHandle<()>>,
    /// Arm gate: set at the start of shutdown, never cleared.
    shutting_down: AtomicBool,
}

impl ChildTaskRuntime {
    /// Build the executor with its dedicated worker thread.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the OS refuses the worker
    /// thread.
    pub(crate) fn new() -> Result<Self, EngineError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("aion-child-tasks")
            .enable_all()
            .build()
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to start the child-task runtime: {error}"),
            })?;
        Ok(Self {
            runtime: Mutex::new(Some(runtime)),
            watches: DashMap::new(),
            spawn_retries: DashMap::new(),
            shutting_down: AtomicBool::new(false),
        })
    }

    /// Arm a child-terminal watcher task for one `(parent pid, child id)`.
    ///
    /// Idempotent per key; refused (returning `false`) once shutdown began.
    pub(crate) fn arm_watch<F>(&self, parent_pid: u64, child_id: WorkflowId, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self::arm(
            &self.shutting_down,
            &self.runtime,
            &self.watches,
            (parent_pid, child_id),
            task,
        )
    }

    /// Arm a spawn-recovery task for one recorded child workflow id.
    ///
    /// Idempotent per child id; refused (returning `false`) once shutdown
    /// began.
    pub(crate) fn arm_spawn_retry<F>(&self, child_id: WorkflowId, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self::arm(
            &self.shutting_down,
            &self.runtime,
            &self.spawn_retries,
            child_id,
            task,
        )
    }

    fn arm<K, F>(
        shutting_down: &AtomicBool,
        runtime: &Mutex<Option<tokio::runtime::Runtime>>,
        registry: &DashMap<K, JoinHandle<()>>,
        key: K,
        task: F,
    ) -> bool
    where
        K: std::hash::Hash + Eq + Clone,
        F: Future<Output = ()> + Send + 'static,
    {
        if shutting_down.load(Ordering::Acquire) {
            return false;
        }
        let handle = {
            let guard = match runtime.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(owned) = guard.as_ref() else {
                return false;
            };
            owned.handle().clone()
        };
        match registry.entry(key) {
            Entry::Occupied(slot) => {
                if slot.get().is_finished() {
                    // A finished task's self-removal can race a re-arm for
                    // the same key; replace the dead handle.
                    let (key, _finished) = slot.replace_entry(handle.spawn(task));
                    let _ = key;
                } else {
                    return false;
                }
                true
            }
            Entry::Vacant(slot) => {
                // The entry guard holds the shard lock, so the task's own
                // completion-time removal blocks until this insert finishes.
                slot.insert(handle.spawn(task));
                true
            }
        }
    }

    /// Drop the registry entry for a finished watcher task.
    pub(crate) fn remove_watch(&self, parent_pid: u64, child_id: &WorkflowId) {
        self.watches.remove(&(parent_pid, child_id.clone()));
    }

    /// Drop the registry entry for a finished spawn-recovery task.
    pub(crate) fn remove_spawn_retry(&self, child_id: &WorkflowId) {
        self.spawn_retries.remove(child_id);
    }

    /// Abort and drop the watcher armed for one `(parent pid, child id)`.
    ///
    /// Used when a `with_timeout` scope expires for an `await_child`: the
    /// aborted await must not let the watcher record the child terminal
    /// into the parent later, or replay would resolve the await against an
    /// arrival the live run never observed (F1).
    pub(crate) fn abort_watch(&self, parent_pid: u64, child_id: &WorkflowId) {
        if let Some((_, handle)) = self.watches.remove(&(parent_pid, child_id.clone())) {
            handle.abort();
        }
    }

    /// Abort and drop every watcher armed by `parent_pid` (process exit).
    pub(crate) fn abort_watches_for_parent(&self, parent_pid: u64) {
        self.watches.retain(|(pid, _), handle| {
            if *pid == parent_pid {
                handle.abort();
                false
            } else {
                true
            }
        });
    }

    /// Number of currently armed watcher tasks.
    #[cfg(test)]
    pub(crate) fn armed_watch_count(&self) -> usize {
        self.watches.len()
    }

    /// Number of currently armed spawn-recovery tasks.
    #[cfg(test)]
    pub(crate) fn armed_spawn_retry_count(&self) -> usize {
        self.spawn_retries.len()
    }

    /// Close the epoch: gate new arms, abort every task, await each aborted
    /// handle to quiescence, then release the owned runtime.
    ///
    /// Blocking is safe in any context: the awaits run on this registry's
    /// own worker thread while the caller blocks on a plain channel, and the
    /// final runtime release uses `shutdown_background` (nothing is running
    /// by then).
    pub(crate) fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.watches.retain(|_, handle| {
            handle.abort();
            false
        });
        self.spawn_retries.retain(|_, handle| {
            handle.abort();
            false
        });
        let runtime = {
            let mut guard = match self.runtime.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.take()
        };
        let Some(runtime) = runtime else {
            return;
        };
        // Nothing new can be spawned (gate above, runtime slot emptied).
        // Quiescence of every aborted task comes from the blocking runtime
        // drop below: it cancels all remaining tasks and waits for in-flight
        // polls to finish before returning, which is the abort-AND-await
        // contract the epoch close requires.
        shutdown_runtime_and_join(runtime);
    }
}

/// Shut the owned runtime down and wait for its worker to finish in-flight
/// polls, from any calling context.
fn shutdown_runtime_and_join(runtime: tokio::runtime::Runtime) {
    // Dropping a `Runtime` inside an async context panics; spawn a plain
    // thread to perform the blocking drop and join it. The drop cancels all
    // remaining tasks and waits for in-flight polls to complete, which is
    // exactly the quiescence guarantee the epoch close needs.
    match std::thread::Builder::new()
        .name("aion-child-tasks-shutdown".to_owned())
        .spawn(move || drop(runtime))
    {
        Ok(joiner) => {
            if joiner.join().is_err() {
                tracing::error!("child-task runtime shutdown thread panicked");
            }
        }
        Err(error) => {
            tracing::error!(
                error = %error,
                "could not spawn the child-task runtime shutdown thread; \
                 falling back to background shutdown"
            );
        }
    }
}

impl Drop for ChildTaskRuntime {
    fn drop(&mut self) {
        // Backstop for an engine dropped without an explicit shutdown: gate,
        // abort everything, and release the runtime without blocking (this
        // can run inside a host async context, where a blocking drop would
        // panic).
        self.shutting_down.store(true, Ordering::Release);
        self.watches.retain(|_, handle| {
            handle.abort();
            false
        });
        self.spawn_retries.retain(|_, handle| {
            handle.abort();
            false
        });
        let runtime = {
            let mut guard = match self.runtime.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.take()
        };
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use aion_core::WorkflowId;

    use super::ChildTaskRuntime;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Sets a flag when the future is dropped (completion or abort).
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn park_forever(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send + 'static {
        // The guard is captured at construction, not at first poll: a task
        // aborted before it ever runs still drops its future, and the flag
        // must observe that.
        let guard = DropFlag(flag);
        async move {
            let _guard = guard;
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    }

    #[test]
    fn arming_is_idempotent_per_key() -> TestResult {
        let tasks = ChildTaskRuntime::new()?;
        let parent = 7;
        let child = WorkflowId::new_v4();
        let flag = Arc::new(AtomicBool::new(false));

        assert!(tasks.arm_watch(parent, child.clone(), park_forever(Arc::clone(&flag))));
        assert!(!tasks.arm_watch(parent, child.clone(), park_forever(Arc::clone(&flag))));
        assert_eq!(tasks.armed_watch_count(), 1);

        // A different child under the same parent is its own watcher.
        assert!(tasks.arm_watch(
            parent,
            WorkflowId::new_v4(),
            park_forever(Arc::clone(&flag))
        ));
        assert_eq!(tasks.armed_watch_count(), 2);
        tasks.shutdown();
        Ok(())
    }

    #[test]
    fn abort_watch_disarms_a_single_key() -> TestResult {
        let tasks = ChildTaskRuntime::new()?;
        let child = WorkflowId::new_v4();
        let other = WorkflowId::new_v4();
        let flag = Arc::new(AtomicBool::new(false));
        assert!(tasks.arm_watch(3, child.clone(), park_forever(Arc::clone(&flag))));
        assert!(tasks.arm_watch(3, other.clone(), park_forever(Arc::clone(&flag))));

        tasks.abort_watch(3, &child);

        assert_eq!(tasks.armed_watch_count(), 1);
        // The remaining key is the other child: re-arming it is still a
        // no-op, re-arming the aborted one is accepted.
        assert!(!tasks.arm_watch(3, other, park_forever(Arc::clone(&flag))));
        assert!(tasks.arm_watch(3, child, park_forever(Arc::clone(&flag))));
        tasks.shutdown();
        Ok(())
    }

    #[test]
    fn abort_for_parent_leaves_other_parents_armed() -> TestResult {
        let tasks = ChildTaskRuntime::new()?;
        let flag = Arc::new(AtomicBool::new(false));
        assert!(tasks.arm_watch(31, WorkflowId::new_v4(), park_forever(Arc::clone(&flag))));
        assert!(tasks.arm_watch(31, WorkflowId::new_v4(), park_forever(Arc::clone(&flag))));
        assert!(tasks.arm_watch(32, WorkflowId::new_v4(), park_forever(Arc::clone(&flag))));

        tasks.abort_watches_for_parent(31);

        assert_eq!(tasks.armed_watch_count(), 1);
        tasks.shutdown();
        Ok(())
    }

    #[test]
    fn shutdown_gates_new_arms_and_awaits_aborted_tasks() -> TestResult {
        let tasks = ChildTaskRuntime::new()?;
        let watch_flag = Arc::new(AtomicBool::new(false));
        let retry_flag = Arc::new(AtomicBool::new(false));
        let child = WorkflowId::new_v4();
        assert!(tasks.arm_watch(9, child.clone(), park_forever(Arc::clone(&watch_flag))));
        assert!(tasks.arm_spawn_retry(child.clone(), park_forever(Arc::clone(&retry_flag))));
        assert_eq!(tasks.armed_spawn_retry_count(), 1);

        tasks.shutdown();

        // Awaited, not just aborted: by the time shutdown returns, both task
        // futures have been dropped to quiescence.
        assert!(
            watch_flag.load(Ordering::Acquire),
            "watcher task must be fully dropped before the epoch closes"
        );
        assert!(
            retry_flag.load(Ordering::Acquire),
            "spawn-retry task must be fully dropped before the epoch closes"
        );
        assert_eq!(tasks.armed_watch_count(), 0);
        assert_eq!(tasks.armed_spawn_retry_count(), 0);
        // The gate holds: nothing can be armed after shutdown.
        assert!(!tasks.arm_watch(9, child.clone(), park_forever(Arc::clone(&watch_flag))));
        assert!(!tasks.arm_spawn_retry(child, park_forever(Arc::clone(&retry_flag))));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_is_safe_from_inside_a_host_async_context() -> TestResult {
        let tasks = ChildTaskRuntime::new()?;
        let flag = Arc::new(AtomicBool::new(false));
        assert!(tasks.arm_watch(11, WorkflowId::new_v4(), park_forever(Arc::clone(&flag))));

        // Engine::shutdown runs in whatever context the embedder calls it
        // from — including a current-thread tokio test like this one.
        tasks.shutdown();

        assert!(flag.load(Ordering::Acquire));
        Ok(())
    }

    #[tokio::test]
    async fn drop_backstop_aborts_without_blocking() -> TestResult {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let tasks = ChildTaskRuntime::new()?;
            assert!(tasks.arm_watch(12, WorkflowId::new_v4(), park_forever(Arc::clone(&flag))));
            // Dropped without shutdown: the backstop must abort the task and
            // release the runtime without panicking in this async context.
        }
        // Background shutdown is asynchronous; the abort lands promptly.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !flag.load(Ordering::Acquire) {
            if std::time::Instant::now() > deadline {
                return Err("drop backstop never aborted the armed task".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(())
    }
}
