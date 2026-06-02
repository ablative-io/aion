//! The core engine. Embeds beamr; owns workflow lifecycle, process-per-workflow management, the supervision tree, .aion module loading, durability and replay (durability module), and timers/signals/queries/children/concurrency (time module). Transport-agnostic.

pub mod activity;
pub mod child;
pub mod concurrency;
pub mod durability;
pub mod engine;
pub mod engine_seam;
pub mod error;
pub mod lifecycle;
pub mod loader;
pub mod query;
pub mod registry;
pub mod runtime;
pub mod signal;
pub mod supervision;
pub mod time;
