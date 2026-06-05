//! Generates Rust modules for the shared protobuf contract.

/// Generates protobuf service modules at build time.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = [
        "proto/common.proto",
        "proto/events.proto",
        "proto/workflow.proto",
        "proto/worker.proto",
    ];

    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    tonic_prost_build::configure().compile_protos(&protos, &["proto"])?;

    Ok(())
}
