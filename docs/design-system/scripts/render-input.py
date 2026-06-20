#!/usr/bin/env python3
"""Assemble stacked-dev input JSON files from the design docs.

Usage:
    # Render specific briefs:
    ./scripts/render-input.py OBS-001
    ./scripts/render-input.py OBS-001 OBS-002 --repo /Users/tom/Developer/ablative/aion

    # Pass brief file paths directly:
    ./scripts/render-input.py docs/design/aion-observability/briefs/OBS-001.json

Searches docs/design/<cluster>/briefs/ for each brief ID.
Writes assembled input alongside the brief: docs/design/<cluster>/briefs/<id>-input.json

Reads repo_url from docs/design/roadmap.json and includes it in the output
so remote workers know where to clone from.

With --repo, searches that repo's design directory instead of the
script's own repo. Without --repo, searches the repo containing this script.
"""

import json
import os
import subprocess
import sys


WORKSPACE_ID = "2d5fdd51-1f25-45a4-8f86-4d4c978d1355"


def find_brief(design_dir, brief_id):
    """Walk all clusters under design_dir to find the brief file."""
    for entry in sorted(os.listdir(design_dir)):
        briefs_dir = os.path.join(design_dir, entry, "briefs")
        brief_path = os.path.join(briefs_dir, f"{brief_id}.json")
        if os.path.isfile(brief_path):
            return entry, brief_path
    return None, None


def load_json(path):
    with open(path) as f:
        return json.load(f)


def get_repo_url(repo_root, design_dir):
    """Get the clone URL: prefer roadmap.json repo_url, fall back to git remote."""
    roadmap_path = os.path.join(design_dir, "roadmap.json")
    if os.path.isfile(roadmap_path):
        roadmap = load_json(roadmap_path)
        if roadmap.get("repo_url"):
            return roadmap["repo_url"]
    try:
        return subprocess.check_output(
            ["git", "-C", repo_root, "remote", "get-url", "origin"],
            text=True, stderr=subprocess.DEVNULL
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def get_default_reviewer(repo_root):
    """Get the default reviewer from the collective workspace."""
    try:
        result = subprocess.check_output(
            ["collective", "--text", "team", "reports", "--manager", "Tom"],
            text=True, stderr=subprocess.DEVNULL, timeout=5
        ).strip()
        for line in result.splitlines():
            parts = line.split()
            if parts:
                return parts[0]
    except (subprocess.CalledProcessError, FileNotFoundError, subprocess.TimeoutExpired):
        pass
    return "Waffles the Terrible"


def assemble(repo_root, design_dir, brief_id):
    cluster, brief_path = find_brief(design_dir, brief_id)
    if not cluster:
        print(f"error: cannot find brief {brief_id} in {design_dir}", file=sys.stderr)
        sys.exit(1)

    cluster_dir = os.path.join(design_dir, cluster)

    brief = load_json(brief_path)
    design = load_json(os.path.join(cluster_dir, "design.json"))
    checklist_raw = load_json(os.path.join(cluster_dir, "checklist.json"))
    stories_raw = load_json(os.path.join(cluster_dir, "stories.json"))

    decisions_path = os.path.join(cluster_dir, "decisions.json")
    if not os.path.isfile(decisions_path):
        decisions_path = os.path.join(design_dir, "decisions.json")
    decisions = load_json(decisions_path) if os.path.isfile(decisions_path) else {"decisions": []}

    all_checklist = []
    for section in checklist_raw.get("sections", []):
        for item in section.get("items", []):
            all_checklist.append({"id": item["id"], "text": item["text"]})

    all_stories = []
    for persona in stories_raw.get("personas", []):
        for story in persona.get("stories", []):
            all_stories.append({"id": story["id"], "text": story["text"]})

    brief_adr_ids = set(brief.get("design_anchor", []))
    adrs = []
    for d in decisions.get("decisions", []):
        if d["id"] in brief_adr_ids:
            adrs.append({
                "id": d["id"],
                "title": d["title"],
                "decision": d["decision"],
                "quote": d.get("quote", ""),
                "decided_by": d.get("decided_by", "Tom"),
            })

    brief_checklist_ids = set(brief.get("checklist", []))
    brief_story_ids = set(brief.get("stories", []))

    repo_url = get_repo_url(repo_root, design_dir)

    result = {
        "repo_root": repo_root,
        "brief_id": brief_id,
        "reviewers": ["Waffles the Terrible"],
        "base_ref": "main",
        "placement": "local",
        "isolation": "worktree",
        "brief_document": brief,
        "resolved_context": {
            "adrs": adrs,
            "checklist": [c for c in all_checklist if c["id"] in brief_checklist_ids],
            "stories": [s for s in all_stories if s["id"] in brief_story_ids],
            "constraints": [{"id": c["id"], "text": c.get("text", c.get("description", ""))} for c in design.get("constraints", [])],
            "intention": design.get("intention", ""),
            "design_path": f"docs/design/{cluster}",
            "provenance": {
                "requested_by": "Tom",
                "quote": design.get("intention", "")[:200],
            },
        },
        "verify_fix_cap": 3,
        "review_cap": 1,
        "round_backoff_ms": 2000,
        "review_deadline_ms": 300000,
        "workspace_id": WORKSPACE_ID,
    }

    if repo_url:
        result["clone_url"] = repo_url

    return result


def main():
    brief_args = []
    repo_flag = None

    i = 1
    while i < len(sys.argv):
        arg = sys.argv[i]
        if arg == "--repo" and i + 1 < len(sys.argv):
            repo_flag = sys.argv[i + 1]
            i += 2
        elif arg in ("--help", "-h"):
            print(__doc__)
            sys.exit(0)
        else:
            brief_args.append(arg)
            i += 1

    if not brief_args:
        print(f"usage: {sys.argv[0]} <brief-id-or-path> [...] [--repo PATH]", file=sys.stderr)
        sys.exit(1)

    default_repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

    for arg in brief_args:
        if os.path.isfile(arg):
            abs_path = os.path.abspath(arg)
            brief_id = os.path.splitext(os.path.basename(abs_path))[0]
            parts = abs_path.split(os.sep)
            try:
                design_idx = parts.index("design")
                repo_root = os.sep.join(parts[:design_idx - 1])
            except ValueError:
                print(f"[{arg}] cannot infer repo from path (no docs/design/ found)", file=sys.stderr)
                continue
        else:
            brief_id = arg
            repo_root = os.path.abspath(repo_flag) if repo_flag else default_repo

        design_dir = os.path.join(repo_root, "docs", "design")
        if not os.path.isdir(design_dir):
            print(f"error: no docs/design/ directory in {repo_root}", file=sys.stderr)
            continue

        try:
            input_obj = assemble(repo_root, design_dir, brief_id)
        except FileNotFoundError as e:
            print(f"[{brief_id}] missing file: {e}", file=sys.stderr)
            continue
        except (KeyError, json.JSONDecodeError) as e:
            print(f"[{brief_id}] bad data: {e}", file=sys.stderr)
            continue

        cluster, _ = find_brief(design_dir, brief_id)
        out_path = os.path.join(design_dir, cluster, "briefs", f"{brief_id}-input.json")
        with open(out_path, "w") as f:
            json.dump(input_obj, f, indent=2)
            f.write("\n")

        print(f"{out_path}")


if __name__ == "__main__":
    main()
