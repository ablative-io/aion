//! Executable entry point for the standalone general-purpose worker.

fn main() -> anyhow::Result<()> {
    general_worker::run()
}
