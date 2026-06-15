#!/usr/bin/env bash
# Clean up worktrees from terminal (failed/completed/cancelled) workflows.
# Removes the worktree directory, yg branch, git branch, and norn sessions.
#
# Usage: ./scripts/clean-stale-worktrees.sh [--dry-run]
#
# Scans all repos that have .yggdrasil-worktrees/ directories for
# stacked-dev-* branches, checks if the corresponding aion workflow
# is terminal, and cleans up if so.

set -euo pipefail

DRY_RUN="${1:-}"
ENDPOINT="${AION_ENDPOINT:-http://127.0.0.1:50051}"

clean_worktree() {
    local repo_root="$1"
    local branch="$2"
    local worktree_path="$repo_root/.yggdrasil-worktrees/$branch"

    if [ "$DRY_RUN" = "--dry-run" ]; then
        echo "[dry-run] would clean: $branch in $repo_root"
        return
    fi

    echo "cleaning: $branch in $repo_root"

    # Remove target directory first (biggest disk win)
    if [ -d "$worktree_path/target" ]; then
        rm -rf "$worktree_path/target"
        echo "  removed target/ (build artifacts)"
    fi

    # Remove worktree
    git -C "$repo_root" worktree remove --force "$worktree_path" 2>/dev/null || true

    # Remove yg branch
    yg branch remove --yes "$branch" 2>/dev/null || true

    # Remove git branch
    git -C "$repo_root" branch -D "$branch" 2>/dev/null || true

    # Remove norn sessions
    for suffix in "" "-scout" "-review"; do
        norn session remove "${branch}${suffix}" 2>/dev/null || true
    done

    echo "  done"
}

# Find all repos with stacked-dev worktrees
for repo in /Users/tom/Developer/ablative/*/; do
    worktree_dir="$repo.yggdrasil-worktrees"
    [ -d "$worktree_dir" ] || continue

    for wt in "$worktree_dir"/stacked-dev-*/; do
        [ -d "$wt" ] || continue
        branch="$(basename "$wt")"

        # Check if any running workflow uses this branch
        is_active=$(aion list --endpoint "$ENDPOINT" 2>/dev/null | python3 -c "
import sys, json, subprocess
runs = json.load(sys.stdin)
for r in runs:
    if r.get('status') != 'Running': continue
    wid = r['workflow_id']
    res = subprocess.run(['aion','describe','--endpoint','$ENDPOINT',wid,'--pretty'],
                         capture_output=True, text=True)
    try:
        d = json.loads(res.stdout)
        for e in d.get('history', []):
            if e.get('type') == 'ChildWorkflowStarted':
                b = e.get('data',{}).get('input',{}).get('workspace',{}).get('branch','')
                if b == '$branch':
                    print('active')
                    sys.exit(0)
    except: pass
print('stale')
" 2>/dev/null)

        if [ "$is_active" = "active" ]; then
            echo "skipping: $branch (active workflow)"
        else
            clean_worktree "$repo" "$branch"
        fi
    done
done

echo "cleanup complete"
