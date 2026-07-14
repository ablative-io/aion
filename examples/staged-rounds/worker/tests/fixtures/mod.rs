//! Re-exports only — the fixture bodies live in [`scratch`].

mod scratch;

pub use scratch::{commit_file_in, git, item, provision_input, scratch_repo};
