//! Focused regressions for Aion-home config discovery and state roots.

use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

use tracing_subscriber::fmt::MakeWriter;

use crate::config::ConfigSource;

use super::{CliOverrides, ServerConfig, aion_home};

const HOME_PROBE: &str = "AION_HOME_TEST_PROBE";
const HOME_EXPECTED: &str = "AION_HOME_TEST_EXPECTED";

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut bytes = self
            .0
            .lock()
            .map_err(|_| io::Error::other("captured log lock poisoned"))?;
        bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        CapturedWriter(Arc::clone(&self.0))
    }
}

impl CapturedLogs {
    fn text(&self) -> Result<String, Box<dyn std::error::Error>> {
        let bytes = self.0.lock().map_err(|_| "captured log lock poisoned")?;
        Ok(String::from_utf8(bytes.clone())?)
    }
}

fn write(path: &Path, contents: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

#[test]
fn discovery_precedence_covers_explicit_local_home_and_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("home");
    let working_dir = scratch.path().join("project");
    fs::create_dir_all(&working_dir)?;
    let home_config = home.join("config.toml");
    let local_config = working_dir.join("aion.toml");
    let explicit_config = scratch.path().join("explicit.toml");

    write(&home_config, "[namespaces]\ndefault = \"home\"\n")?;
    let loaded = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)?;
    assert_eq!(loaded.config.namespaces.default, "home");
    assert_eq!(
        loaded.resolution.source,
        ConfigSource::AionHome(home_config)
    );

    write(&local_config, "[namespaces]\ndefault = \"local\"\n")?;
    let loaded = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)?;
    assert_eq!(loaded.config.namespaces.default, "local");
    assert_eq!(
        loaded.resolution.source,
        ConfigSource::ProjectLocal(local_config.clone())
    );

    write(&explicit_config, "[namespaces]\ndefault = \"explicit\"\n")?;
    let cli = CliOverrides {
        config_path: Some(explicit_config.clone()),
        ..CliOverrides::default()
    };
    let loaded = ServerConfig::load_for_test(&cli, &home, &working_dir)?;
    assert_eq!(loaded.config.namespaces.default, "explicit");
    assert_eq!(
        loaded.resolution.source,
        ConfigSource::Explicit(explicit_config)
    );

    fs::remove_file(local_config)?;
    fs::remove_file(home.join("config.toml"))?;
    let loaded = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)?;
    assert_eq!(loaded.config.namespaces.default, "default");
    assert_eq!(loaded.resolution.source, ConfigSource::BuiltInDefaults);
    Ok(())
}

#[test]
fn malformed_home_config_is_a_loud_typed_failure() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("home");
    let working_dir = scratch.path().join("project");
    fs::create_dir_all(&working_dir)?;
    let config_path = home.join("config.toml");
    write(&config_path, "[store\nbackend = nope")?;

    let error = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)
        .err()
        .ok_or("malformed home config unexpectedly loaded")?;
    let message = error.to_string();
    assert!(message.contains("failed to parse Aion home file"));
    assert!(message.contains(&config_path.display().to_string()));
    Ok(())
}

#[test]
fn unconfigured_paths_resolve_under_home_without_eager_creation()
-> Result<(), Box<dyn std::error::Error>> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("not-created-yet");
    let working_dir = scratch.path().join("project");
    fs::create_dir_all(&working_dir)?;

    let loaded = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)?;
    assert_eq!(
        loaded.config.store.data_dir.as_deref(),
        home.join("data").to_str()
    );
    assert_eq!(
        loaded.config.authoring.workspace_dir.as_deref(),
        Some(home.join("authoring").as_path())
    );
    assert!(
        !home.exists(),
        "config reads must not eagerly create Aion home"
    );
    Ok(())
}

#[test]
fn legacy_directories_guard_only_unconfigured_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("home");
    let working_dir = scratch.path().join("project");
    let legacy_data = working_dir.join("aion-data");
    let legacy_authoring = working_dir.join("aion-authoring");
    fs::create_dir_all(&legacy_data)?;
    fs::create_dir_all(&legacy_authoring)?;

    let loaded = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)?;
    assert_eq!(
        loaded.config.store.data_dir.as_deref(),
        legacy_data.to_str()
    );
    assert_eq!(
        loaded.config.authoring.workspace_dir.as_deref(),
        Some(legacy_authoring.as_path())
    );
    assert_eq!(loaded.resolution.migrations.len(), 2);
    for notice in &loaded.resolution.migrations {
        let line = notice.to_string();
        assert!(line.contains("AION HOME MIGRATION REQUIRED"));
        assert!(line.contains("stop the server, move"));
        assert!(line.contains(&home.display().to_string()));
        assert!(line.contains(&working_dir.display().to_string()));
    }

    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || loaded.resolution.log_startup());
    let logs = captured.text()?;
    assert!(logs.contains("Aion home legacy-directory migration guard active"));
    assert!(logs.contains("AION HOME MIGRATION REQUIRED"));
    assert!(logs.contains(&legacy_data.display().to_string()));
    assert!(logs.contains(&home.join("data").display().to_string()));
    assert!(logs.contains("aion-server configuration resolved"));
    assert!(logs.contains("built-in defaults"));
    assert!(logs.contains("aion-server data root resolved"));
    assert!(logs.contains("aion-server authoring root resolved"));

    write(
        &working_dir.join("aion.toml"),
        "[store]\ndata_dir = \"configured-data\"\n\n[authoring]\nworkspace_dir = \"configured-authoring\"\n",
    )?;
    let loaded = ServerConfig::load_for_test(&CliOverrides::default(), &home, &working_dir)?;
    assert_eq!(
        loaded.config.store.data_dir.as_deref(),
        Some("configured-data")
    );
    assert_eq!(
        loaded.config.authoring.workspace_dir.as_deref(),
        Some(Path::new("configured-authoring"))
    );
    assert!(loaded.resolution.migrations.is_empty());
    Ok(())
}

#[test]
fn aion_home_environment_override_is_respected_everywhere() -> Result<(), Box<dyn std::error::Error>>
{
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("overridden-home");
    let working_dir = scratch.path().join("project");
    fs::create_dir_all(&working_dir)?;
    write(
        &home.join("config.toml"),
        "[namespaces]\ndefault = \"from-home-config\"\n",
    )?;
    let executable = std::env::current_exe()?;
    let status = Command::new(executable)
        .arg("--exact")
        .arg("config::load::home_tests::aion_home_env_probe_child")
        .arg("--nocapture")
        .current_dir(&working_dir)
        .env("AION_HOME", &home)
        .env(HOME_PROBE, "1")
        .env(HOME_EXPECTED, &home)
        .env_remove("AION_STORE_DATA_DIR")
        .env_remove("AION_AUTHORING_WORKSPACE_DIR")
        .status()?;
    assert!(status.success(), "AION_HOME probe child failed");
    Ok(())
}

#[test]
fn missing_home_is_a_typed_error() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let status = Command::new(executable)
        .arg("--exact")
        .arg("config::load::home_tests::aion_home_env_probe_child")
        .arg("--nocapture")
        .env_remove("AION_HOME")
        .env_remove("HOME")
        .env(HOME_PROBE, "missing")
        .status()?;
    assert!(status.success(), "missing-home probe child failed");
    Ok(())
}

#[test]
fn aion_home_env_probe_child() -> Result<(), Box<dyn std::error::Error>> {
    let Some(mode) = std::env::var_os(HOME_PROBE) else {
        return Ok(());
    };
    if mode == "missing" {
        let error = aion_home()
            .err()
            .ok_or("missing AION_HOME and HOME unexpectedly resolved")?;
        assert!(error.to_string().contains("set AION_HOME or HOME"));
        return Ok(());
    }
    let expected = std::env::var_os(HOME_EXPECTED).ok_or("probe omitted expected home")?;
    let expected = std::path::PathBuf::from(expected);
    assert_eq!(aion_home()?, expected);
    let config = ServerConfig::load(&CliOverrides::default())?;
    assert_eq!(config.namespaces.default, "from-home-config");
    assert_eq!(
        config.store.data_dir.as_deref(),
        expected.join("data").to_str()
    );
    assert_eq!(
        config.authoring.workspace_dir.as_deref(),
        Some(expected.join("authoring").as_path())
    );
    Ok(())
}
