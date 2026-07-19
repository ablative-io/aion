//! Dynamic Aion-home defaults, legacy-directory migration guards, and startup provenance.

use std::{
    fmt,
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
}

impl ConfigResolution {
    /// Emit config provenance, resolved roots, and any migration guards in the
    /// server's existing structured startup-log style.
    pub(crate) fn log_startup(&self) {
        for migration in &self.migrations {
            warn!(notice = %migration, "Aion home legacy-directory migration guard active");
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
        let selected = select_default("store data", legacy, home_default, &mut migrations);
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
        ));
    }
    Ok(migrations)
}

fn select_default(
    kind: &'static str,
    legacy: PathBuf,
    home_default: PathBuf,
    migrations: &mut Vec<MigrationNotice>,
) -> PathBuf {
    if legacy.is_dir() {
        migrations.push(MigrationNotice {
            kind,
            legacy: legacy.clone(),
            home_default,
        });
        legacy
    } else {
        home_default
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
