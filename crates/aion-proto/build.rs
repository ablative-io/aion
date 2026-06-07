//! Build-time gRPC stub generation for the shared wire contract.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let proto_files = [
        "proto/common.proto",
        "proto/events.proto",
        "proto/workflow.proto",
        "proto/worker.proto",
    ];

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=proto");
    for proto in &proto_files {
        println!("cargo:rerun-if-changed={proto}");
    }

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut config = tonic_prost_build::Config::new();
    config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .message_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .enum_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .compile_with_config(config, &proto_files, &["proto"])?;

    Ok(())
}
