//! AWL-native package assembly: compiled workflow BEAM + embedded SDK
//! closure → complete format-v1 `.aion` archive bytes, beside (not inside)
//! the legacy `workflow.toml` project pipeline.

mod assemble;
mod bundle;
mod bundle_data;
mod prepare;

pub use assemble::{AssembleError, AwlAssembleOptions, DEFAULT_WORKFLOW_TIMEOUT, assemble_awl};
pub use bundle::{sdk_closure_modules, sdk_closure_version};
pub use prepare::{PrepareAwlError, PreparedAwlPackage, compile_and_assemble_awl};
