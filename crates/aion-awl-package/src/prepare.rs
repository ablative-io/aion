//! Shared direct AWL compile-and-assemble seam.

use std::path::Path;

use aion_awl::{CompileError, CompiledWorkflow};

use crate::{AssembleError, AwlAssembleOptions, assemble_awl};

/// Direct-compiled workflow metadata and its complete `.aion` archive.
#[derive(Debug)]
pub struct PreparedAwlPackage {
    /// Compiler output carrying document-derived identity and contracts.
    pub compiled: CompiledWorkflow,
    /// Complete deterministic archive ready for the deploy loader.
    pub archive: Vec<u8>,
}

/// A refusal from [`compile_and_assemble_awl`].
#[derive(Debug, thiserror::Error)]
pub enum PrepareAwlError {
    /// The direct compiler refused the document.
    #[error(transparent)]
    Compile(#[from] CompileError),
    /// Native package assembly refused the compiler output.
    #[error(transparent)]
    Assemble(#[from] AssembleError),
}

/// Compiles AWL source with the direct bytecode compiler and assembles the
/// result with the document's declared workflow timeout.
///
/// This is the shared native seam used by CLI and server deploy paths. It does
/// not invoke the legacy Gleam emitter or an external toolchain.
///
/// # Errors
///
/// Returns [`PrepareAwlError::Compile`] with the direct compiler's diagnostic
/// unchanged, or [`PrepareAwlError::Assemble`] when archive assembly refuses
/// the compiled output.
pub fn compile_and_assemble_awl(
    source: &str,
    schema_root: &Path,
) -> Result<PreparedAwlPackage, PrepareAwlError> {
    let compiled = aion_awl::compile(source, schema_root)?;
    let archive = assemble_awl(
        &compiled,
        AwlAssembleOptions {
            timeout: compiled.timeout,
        },
    )?;
    Ok(PreparedAwlPackage { compiled, archive })
}
