//! Resolution of the server's central user-level configuration and state root.

use std::path::PathBuf;

use crate::error::ServerError;

/// Resolve the Aion user-level configuration and state directory.
///
/// `AION_HOME` wins when set. Otherwise the directory is `$HOME/.aion`. The
/// returned path is absolute, but is not created: config discovery is read-only,
/// and the store or authoring surface creates it only on first write.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when `AION_HOME` is empty, neither
/// `AION_HOME` nor `HOME` can identify a home directory, or a relative
/// `AION_HOME` cannot be resolved because the current directory is unavailable.
pub fn aion_home() -> Result<PathBuf, ServerError> {
    let configured = std::env::var_os("AION_HOME");
    let path = match configured {
        Some(value) if value.is_empty() => {
            return Err(ServerError::Config {
                message: "AION_HOME must not be empty".to_owned(),
            });
        }
        Some(value) => PathBuf::from(value),
        None => {
            let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) else {
                return Err(ServerError::Config {
                    message: "cannot resolve Aion home: set AION_HOME or HOME".to_owned(),
                });
            };
            PathBuf::from(home).join(".aion")
        }
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|source| ServerError::Config {
                message: format!(
                    "cannot resolve relative AION_HOME against the current directory: {source}"
                ),
            })
    }
}
