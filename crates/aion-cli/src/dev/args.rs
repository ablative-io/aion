//! Command-line arguments for the `aion dev` instant authoring loop.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;

/// Arguments for `aion dev`: watch a workflow project and, on save, rebuild,
/// repackage, and hot-load the new content-hash version into a running server.
#[derive(Args, Clone, Debug)]
pub struct DevArgs {
    /// Workflow project root containing `gleam.toml` and `workflow.toml`. The
    /// project's `src/` tree is watched for changes.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Path to the external `gleam` binary the rebuild step spawns. There is
    /// no default binary (ADR-001): the author names it explicitly, exactly as
    /// the server-side authoring surface requires an operator-named
    /// `[authoring].gleam_path`.
    #[arg(long)]
    pub gleam_path: PathBuf,
    /// Optional debounce window, in milliseconds, applied after the first
    /// change event before a rebuild runs — coalescing the burst of events an
    /// editor emits on a single save into one rebuild. Omitted by default
    /// (ADR-001: no invented interval); when absent every change event drives a
    /// rebuild and the content-hash dedupe makes a redundant rebuild a no-op
    /// load.
    #[arg(long, value_parser = parse_debounce_ms)]
    pub debounce_ms: Option<Duration>,
}

/// Parses a positive millisecond debounce window into a [`Duration`]; rejects a
/// zero window as a no-op the author did not mean to ask for.
fn parse_debounce_ms(raw: &str) -> Result<Duration, String> {
    let millis: u64 = raw
        .parse()
        .map_err(|error| format!("invalid --debounce-ms value `{raw}`: {error}"))?;
    if millis == 0 {
        return Err("--debounce-ms must be a positive number of milliseconds".to_owned());
    }
    Ok(Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_debounce_ms;

    #[test]
    fn debounce_parses_positive_milliseconds() {
        assert_eq!(parse_debounce_ms("250"), Ok(Duration::from_millis(250)));
    }

    #[test]
    fn debounce_rejects_zero_and_non_numeric() {
        assert!(parse_debounce_ms("0").is_err());
        assert!(parse_debounce_ms("nope").is_err());
        assert!(parse_debounce_ms("-5").is_err());
    }
}
