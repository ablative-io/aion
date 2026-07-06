//! Startup loading of the four Norn role profiles from `--profiles-dir`.
//!
//! The profiles are the ROLE DOCTRINE — authored and owned in the yggdrasil
//! repo (`docs/design/remediation-flow/profiles/*.md`, DECISIONS.md D3) and
//! deliberately NOT embedded in this binary: the profile author iterates on
//! them without a worker rebuild. They are read once at startup and a missing
//! or unreadable file is a loud startup failure — a worker that silently ran
//! agents without their doctrine would be worse than one that refused to
//! start.

use std::path::Path;

/// The four role profiles, loaded verbatim.
#[derive(Clone, Debug)]
pub struct Profiles {
    /// `test-author.md` — never sees recommendations, writes fail-first tests.
    pub test_author: String,
    /// `developer.md` — fixes the class, never edits authored tests.
    pub developer: String,
    /// `verifier.md` — adversarial, evidence-per-ruling, never its own work.
    pub verifier: String,
    /// `re-auditor.md` — fresh-context class-level re-audit.
    pub re_auditor: String,
}

/// The profile file each role reads, relative to `--profiles-dir` (the
/// yggdrasil checkout's `docs/design/remediation-flow/profiles/`).
pub const PROFILE_FILES: [(&str, &str); 4] = [
    ("test_author", "test-author.md"),
    ("developer", "developer.md"),
    ("verifier", "verifier.md"),
    ("re_auditor", "re-auditor.md"),
];

/// Load all four profiles from `dir`.
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
    match (
        drained.next(),
        drained.next(),
        drained.next(),
        drained.next(),
    ) {
        (Some(test_author), Some(developer), Some(verifier), Some(re_auditor)) => Ok(Profiles {
            test_author,
            developer,
            verifier,
            re_auditor,
        }),
        // Unreachable: the loop pushed exactly four items; surfaced honestly
        // rather than unwrapped.
        _ => Err("profile loading produced fewer than four profiles".to_owned()),
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
    fn loads_all_four_profiles_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "test-author.md",
            "# Test-Author\nnever sees recommendations",
        );
        write(dir.path(), "developer.md", "# Developer\nfix the class");
        write(dir.path(), "verifier.md", "# Verifier\nevidence per ruling");
        write(dir.path(), "re-auditor.md", "# Re-Auditor\nfresh context");

        let profiles = load(dir.path()).expect("profiles load");
        assert!(profiles.test_author.contains("never sees recommendations"));
        assert!(profiles.developer.contains("fix the class"));
        assert!(profiles.verifier.contains("evidence per ruling"));
        assert!(profiles.re_auditor.contains("fresh context"));
    }

    #[test]
    fn a_missing_profile_is_a_loud_error_naming_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "test-author.md", "doctrine");
        // developer.md missing.
        let error = load(dir.path()).expect_err("must fail");
        assert!(error.contains("developer.md"), "error was: {error}");
    }

    #[test]
    fn an_empty_profile_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "test-author.md", "   \n");
        write(dir.path(), "developer.md", "d");
        write(dir.path(), "verifier.md", "v");
        write(dir.path(), "re-auditor.md", "r");
        let error = load(dir.path()).expect_err("must fail");
        assert!(error.contains("test_author"), "error was: {error}");
        assert!(error.contains("empty"), "error was: {error}");
    }
}
