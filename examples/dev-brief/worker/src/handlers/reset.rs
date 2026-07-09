//! The `reset_workspace` handler: the mechanical post-review restore that
//! re-establishes the worktree's exclusivity after every lens round.

use aion_worker::ActivityFailure;

use crate::paths;
use crate::shell::Shell;
use crate::types::{ResetInput, ResetOutcome};

use super::support::{clip, require_run};

/// `reset_workspace`: the mechanical post-review restore, run after EVERY lens
/// round. `git clean -fd` removes untracked droppings; `git checkout -- .`
/// reverts tracked edits — together re-establishing the worktree at the
/// reviewed branch head before the next developer round or the verify stage.
///
/// `-fd`, deliberately NOT `-fdx`: git-ignored build products (`target/`) are
/// expensive to rebuild and belong to no lens, so they SURVIVE the reset —
/// only working-tree droppings are removed.
///
/// The lenses run only on a green gate, so the tree is provably clean when
/// they start: a non-empty `git status --porcelain` here means a lens WROTE
/// into the shared worktree despite the reviewer tool deny-list. That is
/// recorded as `was_clean: false` with the droppings — evidence for the
/// operator, never a failed run.
///
/// # Errors
///
/// Terminal when the destructive-path guard refuses the target, or when `git`
/// cannot run as infrastructure.
pub fn reset(shell: &Shell, input: ResetInput) -> Result<ResetOutcome, ActivityFailure> {
    let ResetInput {
        repo_root,
        workspace_path,
    } = input;
    // PROVE the target is strictly under the repo's dev-brief worktree root
    // before `git clean -fd`/`git checkout` can delete or overwrite anything.
    paths::guard_destructive_path(&repo_root, &workspace_path)?;

    // Capture the droppings BEFORE the restore: this listing IS the evidence a
    // lens misbehaved. Normally empty.
    let status = require_run(
        shell,
        "git",
        &["-C", &workspace_path, "status", "--porcelain"],
        &workspace_path,
        "git status (pre-reset droppings)",
    )?;
    let droppings: Vec<String> = status
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let was_clean = droppings.is_empty();

    require_run(
        shell,
        "git",
        &["-C", &workspace_path, "clean", "-fd"],
        &workspace_path,
        "git clean -fd (post-review reset)",
    )?;
    require_run(
        shell,
        "git",
        &["-C", &workspace_path, "checkout", "--", "."],
        &workspace_path,
        "git checkout -- . (post-review reset)",
    )?;

    let detail = if was_clean {
        "worktree already clean; the post-review reset was a no-op".to_owned()
    } else {
        format!(
            "post-review reset restored the worktree; a lens had left {} \
             working-tree change(s):\n{}",
            droppings.len(),
            clip(status.output.trim())
        )
    };
    Ok(ResetOutcome {
        was_clean,
        droppings,
        detail,
    })
}
