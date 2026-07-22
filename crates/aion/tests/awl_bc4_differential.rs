//! BC-4 differential harness — every fixture BOTH backends accept runs
//! through a real engine and must produce byte-identical durable event trails
//! after normalizing exactly the five ratified field families.
//!
//! Backends (see `harness.rs`):
//! - Reference: `.awl` → `aion_awl::emit_artifact_in` (Gleam source) →
//!   `gleam build` → `aion_package::package_project` → `Package`.
//! - Direct: `.awl` → `parse` → `mir::lower` → `mir::select` → `.beam` bytes,
//!   spliced as the entry module into the reference package's SDK closure.
//!
//! Both packages run through the SAME `EngineBuilder` shape with one shared
//! deterministic `ActivityDispatcher`, so their trails can only diverge on
//! backend behavior. The covered ratchet (`aion-awl/src/mir/covered.rs`) is
//! the fixture set; refusals are out-of-intersection and reported, never
//! failed. The submodules fan out via `#[path]` (the `runtime_codecs.rs`
//! idiom); every file stays under the 500-code-line law.

#[path = "common/trail_norm.rs"]
mod trail_norm;

#[path = "awl_bc4_differential/abi.rs"]
mod abi;
#[path = "awl_bc4_differential/adversarial.rs"]
mod adversarial;
#[path = "awl_bc4_differential/covered.rs"]
mod covered;
#[path = "awl_bc4_differential/dispatcher.rs"]
mod dispatcher;
#[path = "awl_bc4_differential/driver.rs"]
mod driver;
#[path = "awl_bc4_differential/fixtures.rs"]
mod fixtures;
#[path = "awl_bc4_differential/harness.rs"]
mod harness;
#[path = "awl_bc4_differential/report.rs"]
mod report;
#[path = "awl_bc4_differential/run.rs"]
mod run;
#[path = "awl_bc4_differential/serde_pin.rs"]
mod serde_pin;
