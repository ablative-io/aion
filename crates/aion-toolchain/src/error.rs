//! Error taxonomy for the Gleam authoring toolchain.
//!
//! Every variant carries the offending path or the verbatim compiler
//! diagnostics as structured data, so callers (the `aion-server` authoring
//! endpoint) can map a type error onto an inline 400 distinctly from a spawn
//! failure or a packaging fault.

use std::path::PathBuf;

/// Failures produced while compiling, type-checking, and packaging Gleam
/// workflow source through the external `gleam` binary.
#[derive(thiserror::Error, Debug)]
pub enum ToolchainError {
    /// The configured `gleam` binary could not be spawned (not found on the
    /// configured path, not executable, or the OS refused the process).
    ///
    /// This is an operator-configuration fault, never a caller-correctable
    /// source error: it names the path that was invoked.
    #[error("failed to spawn the gleam binary at `{gleam_path}`: {source}")]
    GleamSpawn {
        /// The `gleam` binary path that could not be spawned.
        gleam_path: PathBuf,
        /// The underlying spawn failure reported by the OS.
        source: std::io::Error,
    },

    /// `gleam build` exited non-zero: the submitted source did not compile or
    /// type-check. The captured compiler output travels back verbatim so the
    /// author sees the real type error inline.
    ///
    /// No `.aion` is produced and no partial package is returned on this path.
    #[error("gleam compilation failed:\n{diagnostics}")]
    TypeCheck {
        /// The verbatim `gleam build` diagnostics (stderr, with any stdout
        /// appended) — the inline type error.
        diagnostics: String,
    },

    /// The submitted source compiled and type-checked, but assembling the
    /// `.aion` archive from the built project failed.
    #[error(transparent)]
    Packaging(#[from] aion_package::PackagingError),

    /// A filesystem operation against the project root failed (reading the
    /// descriptor, writing the submitted source, or resolving a path).
    #[error("filesystem operation on `{path}` failed: {source}")]
    Io {
        /// The path the failing operation targeted.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The project root is not a usable Gleam workflow project, or a request
    /// field was malformed before any build ran (no `gleam.toml`, no
    /// `workflow.toml`, an entry module that escapes the project `src/`
    /// directory, or an entry module name that is not a safe logical name).
    #[error("invalid authoring project: {message}")]
    InvalidProject {
        /// Human-readable description of why the project or request was
        /// rejected, naming the offending field or file.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ToolchainError;

    fn assert_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn toolchain_error_is_send_sync_and_static() {
        assert_send_sync::<ToolchainError>();
    }

    #[test]
    fn type_check_message_carries_the_diagnostics_verbatim() {
        let error = ToolchainError::TypeCheck {
            diagnostics: "error: Type mismatch\n  expected Int, got String".to_owned(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("Type mismatch"));
        assert!(rendered.contains("expected Int, got String"));
    }

    #[test]
    fn spawn_message_names_the_binary_path() {
        let error = ToolchainError::GleamSpawn {
            gleam_path: PathBuf::from("/usr/local/bin/gleam"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert!(error.to_string().contains("/usr/local/bin/gleam"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn io_message_names_the_path() {
        let error = ToolchainError::Io {
            path: PathBuf::from("/work/src/demo.gleam"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert!(error.to_string().contains("/work/src/demo.gleam"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn packaging_error_converts_transparently() {
        let error = ToolchainError::from(aion_package::PackagingError::ConfigMissing {
            root: PathBuf::from("/work"),
        });
        assert_eq!(error.to_string(), "no workflow.toml found in /work");
        assert!(matches!(error, ToolchainError::Packaging(_)));
    }
}
