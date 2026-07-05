//! Library surface of the dev-pipeline norn worker: the handler bodies, the
//! schema constants, the typed shell boundary, and the wire types, exposed
//! so hermetic tests can drive the handlers directly with fake-CLI shims on
//! a private `PATH` (the stacked-dev norn-worker layout).

pub mod handlers;
pub mod schemas;
pub mod shell;
pub mod types;
