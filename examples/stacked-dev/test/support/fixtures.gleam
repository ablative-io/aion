//// Shared fixture loading for the test suite: read a file relative to the
//// package root (`gleam test` runs from the package root) as a string.
////
//// The seeded brief fixtures under `docs/design/brief-dev/briefs/` are
//// authored contracts (P4): tests read them, never write them, and never
//// substitute an invented shape when one fails to load — a missing or
//// unreadable fixture is a loud test failure.

/// Read the file at `path` (absolute, or relative to the test run's working
/// directory — the package root) as UTF-8 text.
@external(erlang, "fixtures_ffi", "read_file")
pub fn read_file(path: String) -> Result(String, String)
