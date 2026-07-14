//! Command-line parsing for the dev-brief worker binary.

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address`.
pub(super) const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";

/// Parsed command-line arguments.
#[derive(Debug)]
pub(super) struct Args {
    pub(super) candidates: Vec<String>,
    pub(super) identity_prefix: String,
    pub(super) ready_file: Option<String>,
    pub(super) norn_bin: String,
    /// The directory the two role profiles are loaded from — required: this
    /// package's `worker/profiles/`.
    pub(super) profiles_dir: String,
}

pub(super) fn parse_args() -> anyhow::Result<Args> {
    let default_norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    parse_args_from(std::env::args().skip(1), default_norn_bin)
}

/// The argument-parsing core, fed an explicit iterator and defaults so tests
/// exercise production logic without touching process globals.
pub(super) fn parse_args_from(
    args: impl IntoIterator<Item = String>,
    default_norn_bin: String,
) -> anyhow::Result<Args> {
    let mut candidates = Vec::new();
    let mut identity_prefix = "dev-brief-worker".to_owned();
    let mut ready_file = None;
    let mut norn_bin = default_norn_bin;
    let mut profiles_dir = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => candidates.push(next_value(&mut args, "--address")?),
            "--identity" => identity_prefix = next_value(&mut args, "--identity")?,
            "--ready-file" => ready_file = Some(next_value(&mut args, "--ready-file")?),
            "--norn-bin" => norn_bin = next_value(&mut args, "--norn-bin")?,
            "--profiles-dir" => profiles_dir = Some(next_value(&mut args, "--profiles-dir")?),
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push(DEFAULT_ADDRESS.to_owned());
    }
    let profiles_dir = profiles_dir.ok_or_else(|| {
        anyhow::anyhow!("--profiles-dir is required (point it at this package's worker/profiles/)")
    })?;
    Ok(Args {
        candidates,
        identity_prefix,
        ready_file,
        norn_bin,
        profiles_dir,
    })
}

/// Take the value for a value-taking flag, bailing clearly when it is missing.
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}
