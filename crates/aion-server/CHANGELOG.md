# Changelog

## Unreleased

### Changed

- **`aion-server` is now a library-only crate**: the standalone binary is
  removed. Install the `aion-cli` crate and run `aion server --config <path>`
  instead — it embeds this crate and behaves identically (same configuration
  loading, startup validation, logging, and exit codes).
- The full run loop is exposed as `aion_server::run(CliOverrides) ->
  ExitCode`: tracing initialization, configuration load/validation, gRPC and
  HTTP transports, and signal-driven graceful drain. `CliOverrides`
  (`aion_server::config`) is the typed equivalent of the former binary's
  command-line flags.
- New typed `ServerError` variants `Transport` (a running transport task
  panicked or was cancelled) and `SignalListener` (a termination-signal
  listener could not be installed) replace the binary's `anyhow` contexts;
  `anyhow` and `clap` are no longer dependencies.
