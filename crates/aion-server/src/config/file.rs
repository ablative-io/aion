//! TOML config file discovery, reading, and parsing.

use std::{fs, path::Path};

use crate::{config::ServerConfig, error::ServerError};

use super::ConfigSource;

const PROJECT_CONFIG_FILE: &str = "aion.toml";
const HOME_CONFIG_FILE: &str = "config.toml";

/// Bytes and provenance from the winning file-discovery layer.
pub(super) struct DiscoveredFile {
    pub(super) bytes: Option<Vec<u8>>,
    pub(super) source: ConfigSource,
}

/// Discover and read server config in the exact order: explicit `--config`,
/// project-local `./aion.toml`, `<AION_HOME>/config.toml`, then built-in defaults.
///
/// Only the first discovered file is read. A missing explicit path and every
/// read error are loud typed failures; a discovered file is never silently
/// skipped in favor of a lower-precedence layer.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when an explicit path is missing or the
/// winning file cannot be read. Parsing and validation happen in the merged
/// loader and are likewise loud typed startup failures.
pub(super) fn discover(
    explicit: Option<&Path>,
    home: &Path,
    working_dir: &Path,
) -> Result<DiscoveredFile, ServerError> {
    if let Some(path) = explicit {
        return read(path, ConfigSource::Explicit(path.to_owned()));
    }
    let project = working_dir.join(PROJECT_CONFIG_FILE);
    if project.exists() {
        return read(&project, ConfigSource::ProjectLocal(project.clone()));
    }
    let user = home.join(HOME_CONFIG_FILE);
    if user.exists() {
        return read(&user, ConfigSource::AionHome(user.clone()));
    }
    Ok(DiscoveredFile {
        bytes: None,
        source: ConfigSource::BuiltInDefaults,
    })
}

fn read(path: &Path, source: ConfigSource) -> Result<DiscoveredFile, ServerError> {
    let bytes = fs::read(path).map_err(|error| ServerError::Config {
        message: format!("failed to read config `{}`: {error}", path.display()),
    })?;
    Ok(DiscoveredFile {
        bytes: Some(bytes),
        source,
    })
}

/// Load and validate a required TOML config file.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when the file is missing, unreadable,
/// unparsable, invalid, or Aion home cannot be resolved for omitted path
/// defaults.
pub fn load_required(path: &Path) -> Result<ServerConfig, ServerError> {
    let bytes = fs::read(path).map_err(|error| ServerError::Config {
        message: format!("failed to read config `{}`: {error}", path.display()),
    })?;
    ServerConfig::from_slice(&bytes).map_err(|error| ServerError::Config {
        message: format!("failed to load config `{}`: {error}", path.display()),
    })
}
