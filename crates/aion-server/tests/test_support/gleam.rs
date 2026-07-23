use std::process::Command;

#[must_use]
pub(crate) fn skip_if_unavailable() -> bool {
    if Command::new("gleam")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return false;
    }

    println!("skipping Gleam-dependent test: `gleam` is not available on PATH");
    true
}
