//! Startup loading of the two Norn role profiles from `--profiles-dir`.
//!
//! The profiles are the ROLE DOCTRINE — shipped with this package under
//! `worker/profiles/` and deliberately NOT embedded in the binary: the
//! profile author iterates on them without a worker rebuild. They are read
//! once at startup and a missing or unreadable file is a loud startup
//! failure — a worker that silently ran agents without their doctrine would
//! be worse than one that refused to start.

use std::path::Path;

/// The two role profiles, loaded verbatim.
#[derive(Clone, Debug)]
pub struct Profiles {
    /// `developer.md` — implements the brief in the worktree; never runs git
    /// or the gates (the machinery does).
    pub developer: String,
    /// `reviewer.md` — one adversarial lens; refutes with constructed
    /// evidence, never rubber-stamps.
    pub reviewer: String,
}

/// The profile file each role reads, relative to `--profiles-dir` (this
/// package's `worker/profiles/`).
pub const PROFILE_FILES: [(&str, &str); 2] =
    [("developer", "developer.md"), ("reviewer", "reviewer.md")];

/// Load both profiles from `dir`.
///
/// # Errors
///
/// A message naming the exact file that is missing or unreadable — every
/// role's doctrine is required; there is no default profile.
pub fn load(dir: &Path) -> Result<Profiles, String> {
    let mut loaded: Vec<String> = Vec::with_capacity(PROFILE_FILES.len());
    for (role, file) in PROFILE_FILES {
        let path = dir.join(file);
        let text = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "could not load the {role} profile from {}: {error}",
                path.display()
            )
        })?;
        if text.trim().is_empty() {
            return Err(format!(
                "the {role} profile at {} is empty — refusing to run the role \
                 without its doctrine",
                path.display()
            ));
        }
        loaded.push(text);
    }
    let mut drained = loaded.into_iter();
    match (drained.next(), drained.next()) {
        (Some(developer), Some(reviewer)) => Ok(Profiles {
            developer,
            reviewer,
        }),
        // Unreachable: the loop pushed exactly two items; surfaced honestly
        // rather than unwrapped.
        _ => Err("profile loading produced fewer than two profiles".to_owned()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::load;

    fn write(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write profile");
    }

    #[test]
    fn loads_both_profiles_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "developer.md",
            "# Developer\nimplement the brief",
        );
        write(
            dir.path(),
            "reviewer.md",
            "# Reviewer\nrefute with evidence",
        );

        let profiles = load(dir.path()).expect("profiles load");
        assert!(profiles.developer.contains("implement the brief"));
        assert!(profiles.reviewer.contains("refute with evidence"));
    }

    #[test]
    fn a_missing_profile_is_a_loud_error_naming_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "developer.md", "doctrine");
        // reviewer.md missing.
        let error = load(dir.path()).expect_err("must fail");
        assert!(error.contains("reviewer.md"), "error was: {error}");
    }

    #[test]
    fn an_empty_profile_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "developer.md", "   \n");
        write(dir.path(), "reviewer.md", "r");
        let error = load(dir.path()).expect_err("must fail");
        assert!(error.contains("developer"), "error was: {error}");
        assert!(error.contains("empty"), "error was: {error}");
    }
}
