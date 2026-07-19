//! Dynamic Aion-home defaults, legacy-directory migration guards, and startup provenance.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};
use tracing::{info, warn};

use crate::error::ServerError;

use super::{
    DEFAULT_AUTHORING_WORKSPACE_DIR, DEFAULT_HAEMATITE_DATA_DIR, LEGACY_AUTHORING_WORKSPACE_DIR,
    LEGACY_HAEMATITE_DATA_DIR, ServerConfig,
};

/// The winning file layer in server configuration discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConfigSource {
    /// An explicit `--config PATH`.
    Explicit(PathBuf),
    /// Project-local `./aion.toml`.
    ProjectLocal(PathBuf),
    /// User-level `<AION_HOME>/config.toml`.
    AionHome(PathBuf),
    /// No file was discovered.
    BuiltInDefaults,
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit(path) => write!(formatter, "explicit file `{}`", path.display()),
            Self::ProjectLocal(path) => {
                write!(formatter, "project-local file `{}`", path.display())
            }
            Self::AionHome(path) => write!(formatter, "Aion home file `{}`", path.display()),
            Self::BuiltInDefaults => formatter.write_str("built-in defaults"),
        }
    }
}

/// One legacy default retained to avoid silently stranding durable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationNotice {
    kind: &'static str,
    legacy: PathBuf,
    home_default: PathBuf,
}

impl fmt::Display for MigrationNotice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "AION HOME MIGRATION REQUIRED: using legacy {} directory `{}` instead of new Aion-home default `{}`; stop the server, move `{}` to `{}`, then remove the legacy directory to complete migration",
            self.kind,
            self.legacy.display(),
            self.home_default.display(),
            self.legacy.display(),
            self.home_default.display(),
        )
    }
}

/// Startup-visible provenance and resolved roots accompanying a loaded config.
#[derive(Clone, Debug)]
pub(crate) struct ConfigResolution {
    pub(crate) home: PathBuf,
    pub(crate) source: ConfigSource,
    pub(crate) data_dir: Option<String>,
    pub(crate) authoring_workspace: Option<PathBuf>,
    pub(crate) migrations: Vec<MigrationNotice>,
    #[cfg(not(unix))]
    pub(crate) home_explicit: bool,
    #[cfg(not(unix))]
    pub(crate) data_dir_explicit: bool,
    #[cfg(not(unix))]
    pub(crate) data_root_required: bool,
    #[cfg(not(unix))]
    pub(crate) authoring_workspace_explicit: bool,
}

impl ConfigResolution {
    /// Emit config provenance, resolved roots, and any migration guards in the
    /// server's existing structured startup-log style.
    pub(crate) fn validate_private_home(&self) -> Result<(), ServerError> {
        #[cfg(unix)]
        {
            crate::filesystem::validate_private_root(&self.home, "Aion home").map_err(|error| {
                ServerError::Config {
                    message: format!("unsafe Aion home: {error}"),
                }
            })?;
        }
        #[cfg(not(unix))]
        {
            require_explicit_non_unix_root(
                self.home_explicit,
                "Aion home",
                "AION_HOME",
                &self.home,
            )?;
            if self.data_root_required {
                let data_dir = self
                    .data_dir
                    .as_deref()
                    .ok_or_else(|| ServerError::Config {
                        message: "store.data_dir is required for the haematite backend".to_owned(),
                    })?;
                require_explicit_non_unix_root(
                    self.data_dir_explicit,
                    "data root",
                    "store.data_dir or AION_STORE_DATA_DIR",
                    Path::new(data_dir),
                )?;
            }
            if let Some(authoring) = &self.authoring_workspace {
                require_explicit_non_unix_root(
                    self.authoring_workspace_explicit,
                    "authoring and authoring-state root",
                    "authoring.workspace_dir or AION_AUTHORING_WORKSPACE_DIR",
                    authoring,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn log_startup(&self) {
        for migration in &self.migrations {
            warn!(notice = %migration, "Aion home legacy-directory migration guard active");
        }
        #[cfg(not(unix))]
        {
            warn_unverified_acl("Aion home", &self.home);
            if self.data_root_required {
                if let Some(data_dir) = &self.data_dir {
                    warn_unverified_acl("data root", Path::new(data_dir));
                }
            }
            if let Some(authoring) = &self.authoring_workspace {
                warn_unverified_acl("authoring and authoring-state root", authoring);
            }
        }
        info!(
            config_source = %self.source,
            aion_home = %self.home.display(),
            "aion-server configuration resolved"
        );
        info!(
            data_root = self.data_dir.as_deref().unwrap_or("disabled"),
            "aion-server data root resolved"
        );
        if let Some(path) = &self.authoring_workspace {
            info!(authoring_root = %path.display(), "aion-server authoring root resolved");
        } else {
            info!(
                authoring_root = "disabled",
                "aion-server authoring root resolved"
            );
        }
    }
}

#[cfg(not(unix))]
fn require_explicit_non_unix_root(
    explicit: bool,
    label: &str,
    configuration: &str,
    path: &Path,
) -> Result<(), ServerError> {
    require_explicit_root_selection(explicit, label, configuration, path)?;
    crate::filesystem::validate_private_root(path, label).map_err(|error| ServerError::Config {
        message: format!("unsafe explicitly configured {label}: {error}"),
    })
}

#[cfg(any(not(unix), test))]
fn require_explicit_root_selection(
    explicit: bool,
    label: &str,
    configuration: &str,
    path: &Path,
) -> Result<(), ServerError> {
    if !explicit {
        return Err(ServerError::Config {
            message: format!(
                "refusing default-sensitive {label} `{}` on this non-Unix platform because Aion cannot verify or install a private ACL; pre-provision a private directory and explicitly configure it with {configuration}",
                path.display()
            ),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn warn_unverified_acl(label: &str, path: &Path) {
    warn!(
        sensitive_root = label,
        path = %path.display(),
        "ACL PRIVACY NOT VERIFIED: using explicitly configured sensitive root on a non-Unix platform; Aion does not install or validate an owner-only ACL"
    );
}

/// Apply dynamic home-rooted defaults and the two legacy-directory guards.
///
/// A path is eligible for migration only while its config field is absent. File
/// and environment values are therefore final before this function runs, and an
/// explicitly configured value can never activate a legacy fallback.
pub(super) fn fill_home_defaults(
    config: &mut ServerConfig,
    home: &Path,
    working_dir: &Path,
) -> Result<Vec<MigrationNotice>, ServerError> {
    let mut migrations = Vec::new();
    if config.store.data_dir.is_none() {
        let home_default = home.join(DEFAULT_HAEMATITE_DATA_DIR);
        let legacy = working_dir.join(LEGACY_HAEMATITE_DATA_DIR);
        let selected = select_default("store data", legacy, home_default, &mut migrations)?;
        config.store.data_dir = Some(path_to_string(&selected, "store.data_dir")?);
    }
    if config.authoring.workspace_dir.is_none() {
        let home_default = home.join(DEFAULT_AUTHORING_WORKSPACE_DIR);
        let legacy = working_dir.join(LEGACY_AUTHORING_WORKSPACE_DIR);
        config.authoring.workspace_dir = Some(select_default(
            "authoring workspace",
            legacy,
            home_default,
            &mut migrations,
        )?);
    }
    Ok(migrations)
}

fn select_default(
    kind: &'static str,
    legacy: PathBuf,
    home_default: PathBuf,
    migrations: &mut Vec<MigrationNotice>,
) -> Result<PathBuf, ServerError> {
    let is_real_directory = match std::fs::symlink_metadata(&legacy) {
        Ok(metadata) => metadata.is_dir() && !metadata.file_type().is_symlink(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(ServerError::Config {
                message: format!(
                    "failed to inspect legacy {kind} path `{}`: {error}",
                    legacy.display()
                ),
            });
        }
    };
    if is_real_directory {
        migrations.push(MigrationNotice {
            kind,
            legacy: legacy.clone(),
            home_default,
        });
        Ok(legacy)
    } else {
        Ok(home_default)
    }
}

fn path_to_string(path: &Path, field: &str) -> Result<String, ServerError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ServerError::Config {
            message: format!(
                "resolved {field} path `{}` is not valid UTF-8; configure {field} explicitly with a UTF-8 path",
                path.display()
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_unix_default_sensitive_roots_fail_with_explicit_acl_remediation()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new(r"C:\ProgramData\Aion");
        let error = require_explicit_root_selection(false, "Aion home", "AION_HOME", path)
            .err()
            .ok_or("a non-Unix default root did not fail closed")?;
        let message = error.to_string();
        assert!(message.contains("default-sensitive Aion home"));
        assert!(message.contains("cannot verify or install a private ACL"));
        assert!(message.contains("pre-provision a private directory"));
        assert!(message.contains("AION_HOME"));
        Ok(())
    }

    #[test]
    fn non_unix_explicit_sensitive_root_selection_is_accepted_for_shape_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        require_explicit_root_selection(
            true,
            "data root",
            "store.data_dir or AION_STORE_DATA_DIR",
            Path::new(r"C:\Aion\data"),
        )?;
        Ok(())
    }
}
