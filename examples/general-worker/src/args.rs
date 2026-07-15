//! Strict command-line parsing for the worker binary.

use std::fmt::{Display, Formatter};
use std::path::PathBuf;

/// Default liminal server address when no `--address` is supplied.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";

/// Parsed worker command-line arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    /// Liminal address candidates, in operator-supplied order.
    pub addresses: Vec<String>,
    /// Identity prefix used to derive unique node identities.
    pub identity: String,
    /// Optional readiness marker written after shell-node registration.
    pub ready_file: Option<PathBuf>,
    /// Norn executable name or path.
    pub norn_bin: String,
}

/// Loud command-line validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgsError {
    message: String,
}

impl ArgsError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ArgsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ArgsError {}

/// Parse process arguments and use `NORN_BIN` as the Norn default.
///
/// # Errors
///
/// Returns [`ArgsError`] for a non-Unicode `NORN_BIN`, missing or blank flag
/// values, or any unknown flag.
pub fn parse_args() -> Result<Args, ArgsError> {
    let default_norn_bin = match std::env::var("NORN_BIN") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "norn".to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(ArgsError::new("NORN_BIN must contain valid Unicode"));
        }
    };
    parse_args_from(std::env::args().skip(1), default_norn_bin)
}

/// Parse an injected argument iterator with an injected Norn default.
///
/// # Errors
///
/// Returns [`ArgsError`] for missing or blank flag values and unknown flags.
pub fn parse_args_from(
    args: impl IntoIterator<Item = String>,
    default_norn_bin: String,
) -> Result<Args, ArgsError> {
    let mut addresses = Vec::new();
    let mut identity = "general-worker".to_owned();
    let mut ready_file = None;
    let mut norn_bin = default_norn_bin;
    let mut arguments = args.into_iter();

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--address" => addresses.push(next_value(&mut arguments, "--address")?),
            "--identity" => identity = next_value(&mut arguments, "--identity")?,
            "--ready-file" => {
                ready_file = Some(PathBuf::from(next_value(&mut arguments, "--ready-file")?));
            }
            "--norn-bin" => norn_bin = next_value(&mut arguments, "--norn-bin")?,
            unknown => return Err(ArgsError::new(format!("unknown argument `{unknown}`"))),
        }
    }

    if addresses.is_empty() {
        addresses.push(DEFAULT_ADDRESS.to_owned());
    }
    if norn_bin.trim().is_empty() {
        return Err(ArgsError::new(
            "Norn binary from `--norn-bin` or `NORN_BIN` must be nonblank",
        ));
    }

    Ok(Args {
        addresses,
        identity,
        ready_file,
        norn_bin,
    })
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<String, ArgsError> {
    let value = arguments
        .next()
        .ok_or_else(|| ArgsError::new(format!("{flag} requires a value")))?;
    if value.trim().is_empty() {
        return Err(ArgsError::new(format!("{flag} requires a nonblank value")));
    }
    Ok(value)
}
