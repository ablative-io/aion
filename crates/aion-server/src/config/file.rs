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
    if is_present(&project)? {
        return read(&project, ConfigSource::ProjectLocal(project.clone()));
    }
    let user = home.join(HOME_CONFIG_FILE);
    if is_present(&user)? {
        return read(&user, ConfigSource::AionHome(user.clone()));
    }
    Ok(DiscoveredFile {
        bytes: None,
        source: ConfigSource::BuiltInDefaults,
    })
}

fn is_present(path: &Path) -> Result<bool, ServerError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ServerError::Config {
            message: format!(
                "failed to inspect config path `{}`: {error}",
                path.display()
            ),
        }),
    }
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

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    #[test]
    fn dangling_project_config_is_loud_and_home_is_not_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let sandbox = tempfile::tempdir()?;
        let working = sandbox.path().join("working");
        let home = sandbox.path().join("home");
        fs::create_dir(&working)?;
        fs::create_dir(&home)?;
        fs::write(home.join(HOME_CONFIG_FILE), b"not valid toml =")?;
        symlink(
            sandbox.path().join("missing"),
            working.join(PROJECT_CONFIG_FILE),
        )?;

        let error = discover(None, &home, &working)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains(PROJECT_CONFIG_FILE));
        assert!(!error.to_string().contains("parse"));
        Ok(())
    }

    #[test]
    fn dangling_home_config_is_loud_instead_of_defaults() -> Result<(), Box<dyn std::error::Error>>
    {
        let sandbox = tempfile::tempdir()?;
        let working = sandbox.path().join("working");
        let home = sandbox.path().join("home");
        fs::create_dir(&working)?;
        fs::create_dir(&home)?;
        symlink(sandbox.path().join("missing"), home.join(HOME_CONFIG_FILE))?;

        let error = discover(None, &home, &working)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains(HOME_CONFIG_FILE));
        Ok(())
    }

    #[test]
    fn unstatable_project_and_home_paths_are_loud() -> Result<(), Box<dyn std::error::Error>> {
        let sandbox = tempfile::tempdir()?;
        let working = sandbox.path().join("working");
        let home = sandbox.path().join("home");
        fs::create_dir(&working)?;
        fs::create_dir(&home)?;
        fs::write(home.join(HOME_CONFIG_FILE), b"lower layer must not win")?;

        fs::set_permissions(&working, fs::Permissions::from_mode(0o000))?;
        let project_result = discover(None, &home, &working);
        fs::set_permissions(&working, fs::Permissions::from_mode(0o700))?;
        assert!(
            project_result.is_err(),
            "project metadata failure was suppressed"
        );

        fs::set_permissions(&home, fs::Permissions::from_mode(0o000))?;
        let home_result = discover(None, &home, &working);
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        assert!(home_result.is_err(), "home metadata failure was suppressed");
        Ok(())
    }
}
