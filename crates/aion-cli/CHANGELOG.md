# Changelog

## Unreleased

### Changed

- **The installed binary is now named `aion`** (was `aion-cli`). The crate
  name stays `aion-cli`; `cargo install aion-cli` installs the `aion`
  executable. All subcommands and global flags are unchanged.
- **New `aion server` subcommand** runs the full Aion workflow server
  in-process — the Temporal-style unified binary. It exposes exactly the
  surface of the former standalone `aion-server` binary (`--config`,
  `--listen-address`, `--store-url`, `--scheduler-threads`,
  `--drain-timeout`, repeatable `--workflow-package`) and preserves its
  operational contract: JSON tracing logs, exit code 2 for configuration
  errors, graceful drain on the first termination signal, and exit code 130
  when a second signal forces immediate exit.
- New `auth` feature forwards to `aion-server/auth` for JWT/JWKS
  authentication in the embedded server. Off by default, matching the
  feature set the standalone server binary shipped with.
- The default cancellation reason for `aion cancel` is now
  `cancelled by aion` (was `cancelled by aion-cli`), matching the binary
  name.
