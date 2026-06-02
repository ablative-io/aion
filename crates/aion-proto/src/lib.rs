//! The shared wire contract: gRPC service definitions and serde wire types used by the server, all client SDKs, and all worker SDKs. Depends only on aion-core for type parity.

pub mod convert;
pub mod error;
