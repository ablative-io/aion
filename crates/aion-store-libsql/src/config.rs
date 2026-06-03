//! Deserialize-only configuration for the libSQL event store.

use std::path::PathBuf;

use serde::Deserialize;

/// Operator-provided libSQL settings.
///
/// Durability, WAL, and replica sync values are optional because this crate does not assume
/// defaults for operator tunables; absent values remain unset until connection code applies only
/// the values explicitly provided here.
#[derive(Debug, Deserialize)]
pub struct LibSqlConfig {
    /// Connection mode for the embedded libSQL store.
    pub mode: LibSqlMode,
    /// Optional libSQL journal mode, such as a WAL mode chosen by the operator.
    pub journal_mode: Option<String>,
    /// Optional libSQL synchronous setting chosen by the operator.
    pub synchronous: Option<String>,
    /// Optional replica sync interval in seconds, used only for embedded-replica mode.
    pub sync_interval_seconds: Option<u64>,
}

/// Selects whether libSQL opens a local embedded file or an embedded replica.
#[derive(Debug, Deserialize)]
pub enum LibSqlMode {
    /// Embedded local-file mode.
    Embedded {
        /// Path to the local libSQL database file.
        path: PathBuf,
    },
    /// Embedded replica mode, using a local file synchronized with a remote primary.
    EmbeddedReplica {
        /// Path to the local replica database file.
        path: PathBuf,
        /// Remote primary URL used by libSQL replica sync.
        primary_url: String,
        /// Authentication token for the remote primary.
        auth_token: String,
    },
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{LibSqlConfig, LibSqlMode};

    #[test]
    fn deserializes_embedded_mode() -> Result<(), Box<dyn std::error::Error>> {
        let config: LibSqlConfig = serde_json::from_str(
            r#"{
                "mode": {
                    "Embedded": {
                        "path": "app.db"
                    }
                },
                "journal_mode": "wal",
                "synchronous": "normal",
                "sync_interval_seconds": 15
            }"#,
        )?;

        match config.mode {
            LibSqlMode::Embedded { path } => assert_eq!(path, Path::new("app.db")),
            LibSqlMode::EmbeddedReplica { .. } => {
                return Err("expected embedded mode".into());
            }
        }
        assert_eq!(config.journal_mode.as_deref(), Some("wal"));
        assert_eq!(config.synchronous.as_deref(), Some("normal"));
        assert_eq!(config.sync_interval_seconds, Some(15));

        Ok(())
    }

    #[test]
    fn deserializes_embedded_replica_mode() -> Result<(), Box<dyn std::error::Error>> {
        let config: LibSqlConfig = serde_json::from_str(
            r#"{
                "mode": {
                    "EmbeddedReplica": {
                        "path": "replica.db",
                        "primary_url": "libsql://primary.example.com",
                        "auth_token": "secret-token"
                    }
                }
            }"#,
        )?;

        match config.mode {
            LibSqlMode::EmbeddedReplica {
                path,
                primary_url,
                auth_token,
            } => {
                assert_eq!(path, Path::new("replica.db"));
                assert_eq!(primary_url, "libsql://primary.example.com");
                assert_eq!(auth_token, "secret-token");
            }
            LibSqlMode::Embedded { .. } => {
                return Err("expected embedded-replica mode".into());
            }
        }

        Ok(())
    }

    #[test]
    fn embedded_replica_requires_primary_url() {
        let result = serde_json::from_str::<LibSqlConfig>(
            r#"{
                "mode": {
                    "EmbeddedReplica": {
                        "path": "replica.db",
                        "auth_token": "secret-token"
                    }
                }
            }"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn omitted_tunables_remain_unset() -> Result<(), Box<dyn std::error::Error>> {
        let config: LibSqlConfig = serde_json::from_str(
            r#"{
                "mode": {
                    "Embedded": {
                        "path": "app.db"
                    }
                }
            }"#,
        )?;

        assert!(config.journal_mode.is_none());
        assert!(config.synchronous.is_none());
        assert!(config.sync_interval_seconds.is_none());

        Ok(())
    }
}
