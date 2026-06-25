//! Machine-generated tonic/prost gRPC stubs compiled from the Aion proto
//! contract.
//!
//! This crate exists solely to isolate the generated code from the
//! hand-written `aion-proto` crate. Because the stubs are produced by
//! tonic/prost and not maintained by hand, their relaxed lint posture is
//! expressed in this crate's `Cargo.toml` `[lints]` section instead of an
//! in-source lint-suppression attribute. Consumers should depend on
//! `aion-proto` with the `generated` feature, which re-exports this crate as
//! `aion_proto::generated`.

tonic::include_proto!("aion");
