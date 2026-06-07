//! TOML config file discovery, reading, and parsing.

use std::{fs, path::Path};

use crate::{config::ServerConfig, error::ServerError};

const DEFAULT_CONFIG_FILE: &str = "aion.toml";

/// Load the explicitly requested config file or the default `aion.toml` when present.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when an explicit path is missing or when a present file cannot
/// be read, parsed, or validated.
pub fn load(path: Option<&Path>) -> Result<Option<ServerConfig>, ServerError> {
    if let Some(path) = path {
        load_required(path).map(Some)
    } else {
        let default_path = Path::new(DEFAULT_CONFIG_FILE);
        if default_path.exists() {
            load_required(default_path).map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Load and validate a required TOML config file.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when the file is missing, unreadable, unparsable, or invalid.
pub fn load_required(path: &Path) -> Result<ServerConfig, ServerError> {
    let bytes = fs::read(path).map_err(|source| ServerError::Config {
        message: format!("failed to read config `{}`: {source}", path.display()),
    })?;
    ServerConfig::from_slice(&bytes)
}
