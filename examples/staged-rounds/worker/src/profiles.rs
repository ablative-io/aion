//! Startup loading of the four Norn role profiles from `--profiles-dir`.
//!
//! The profiles are the ROLE DOCTRINE — shipped with this package under
//! `worker/profiles/` and deliberately NOT embedded in the binary: the
//! profile author iterates on them without a worker rebuild. They are read
//! once at startup and a missing or unreadable file is a loud startup
//! failure — a worker that silently ran agents without their doctrine would
//! be worse than one that refused to start.

use std::path::Path;

/// The four role profiles, loaded verbatim.
#[derive(Clone, Debug)]
pub struct Profiles {
    /// `planner.md` — decomposes the material into parallel-safe,
    /// scope-fenced phased items; never writes files.
    pub planner: String,
    /// `developer.md` — implements one item in its worktree; never runs git
    /// (the machinery commits).
    pub developer: String,
    /// `reviewer.md` — one adversarial per-item reviewer; refutes with
    /// constructed evidence, never rubber-stamps.
    pub reviewer: String,
    /// `remediator.md` — resolves merge conflicts preserving both items'
    /// intent; minimal edits, no new features.
    pub remediator: String,
}

/// The profile file each role reads, relative to `--profiles-dir` (this
/// package's `worker/profiles/`).
pub const PROFILE_FILES: [(&str, &str); 4] = [
    ("planner", "planner.md"),
    ("developer", "developer.md"),
    ("reviewer", "reviewer.md"),
    ("remediator", "remediator.md"),
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
        (Some(planner), Some(developer), Some(reviewer), Some(remediator)) => Ok(Profiles {
            planner,
            developer,
            reviewer,
            remediator,
        }),
        // Unreachable: the loop pushed exactly four items; surfaced honestly
        // rather than unwrapped.
        _ => Err("profile loading produced fewer than four profiles".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::load;

    fn write_all(dir: &std::path::Path) -> std::io::Result<()> {
        for (name, body) in [
            ("planner.md", "# Planner\ndecompose the material"),
            ("developer.md", "# Developer\nimplement the item"),
            ("reviewer.md", "# Reviewer\nrefute with evidence"),
            ("remediator.md", "# Remediator\npreserve both intents"),
        ] {
            std::fs::write(dir.join(name), body)?;
        }
        Ok(())
    }

    #[test]
    fn loads_all_four_profiles_verbatim() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        write_all(dir.path())?;
        let profiles = load(dir.path()).map_err(anyhow::Error::msg)?;
        assert!(profiles.planner.contains("decompose the material"));
        assert!(profiles.developer.contains("implement the item"));
        assert!(profiles.reviewer.contains("refute with evidence"));
        assert!(profiles.remediator.contains("preserve both intents"));
        Ok(())
    }

    #[test]
    fn a_missing_profile_is_a_loud_error_naming_the_file() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        write_all(dir.path())?;
        std::fs::remove_file(dir.path().join("remediator.md"))?;
        let Err(error) = load(dir.path()) else {
            anyhow::bail!("profiles unexpectedly loaded without remediator.md");
        };
        assert!(error.contains("remediator.md"), "error was: {error}");
        Ok(())
    }

    #[test]
    fn an_empty_profile_is_refused() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        write_all(dir.path())?;
        std::fs::write(dir.path().join("planner.md"), "   \n")?;
        let Err(error) = load(dir.path()) else {
            anyhow::bail!("profiles unexpectedly loaded with an empty planner profile");
        };
        assert!(error.contains("planner"), "error was: {error}");
        assert!(error.contains("empty"), "error was: {error}");
        Ok(())
    }
}
