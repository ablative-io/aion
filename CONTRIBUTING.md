# Contributing

## Local workflow artifacts

Some development tools create local process state while you work in this repository. These files are intentionally gitignored and should not be committed:

- `.yggdrasil-worktrees/` — temporary workflow worktrees created by orchestration tooling.
- `.meridian/` — local Meridian/Norn state, profiles, tasks, hooks, and workflow metadata.
- `.claude/` — local Claude Code settings, skills, and process configuration.
- `server.log` — local server output from development runs.
- `.commit-msg.tmp` — temporary commit-message scratch file.

These artifacts are machine- or session-specific. Keep them on disk when your local tools need them, but treat them as local-only process files rather than repository source.
