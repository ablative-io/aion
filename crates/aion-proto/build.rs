//! Build-time gRPC stub generation for the shared wire contract.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut config = tonic_prost_build::Config::new();
    config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .message_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .enum_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .compile_with_config(config, &["proto/workflow.proto"], &["proto"])?;
    Ok(())
}
