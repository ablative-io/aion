#!/usr/bin/env python3
"""Check coverage of checklist items and user stories across briefs.

Usage:
  check-coverage.py <cluster-dir>

Checks (FAIL = exit 1):
  - Checklist items assigned to no brief
  - User stories assigned to no brief
  - Unknown C#/S# ids referenced by briefs
  - Per-brief bidirectional coverage: the brief-level checklist/stories
    arrays must equal the union of the per-R# arrays (both directions of
    mismatch are reported)
  - Every R# files path (create/modify/delete) appears in the cluster
    design.json structure array
  - Every design_anchor ADR id exists in decisions.json (found by
    searching upward from the cluster dir)
  - Dependency cycles among briefs

Warnings (reported, not failures):
  - Items claimed by multiple briefs (splits are legal when noted)
  - depends_on brief ids not found in this cluster (may be cross-cluster)

Also prints the brief dependency chain.
"""
import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def _id_sort_key(item_id: str):
    digits = "".join(ch for ch in item_id if ch.isdigit())
    return (int(digits) if digits else 0, item_id)


def load_json(path: Path) -> dict | None:
    try:
        with open(path) as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"error: cannot read {path}: {exc}", file=sys.stderr)
        return None


def find_decisions_path(start: Path) -> Path | None:
    current = start.resolve()
    for candidate in (current, *current.parents):
        ledger = candidate / "decisions.json"
        if ledger.is_file():
            return ledger
    return None


def path_in_structure(path: str, structure_paths: list[str]) -> bool:
    """True when path matches a structure entry exactly or sits under a
    structure entry that names a directory."""
    normalized = path.rstrip("/")
    for entry in structure_paths:
        entry_norm = entry.rstrip("/")
        if normalized == entry_norm or normalized.startswith(entry_norm + "/"):
            return True
    return False


def detect_cycles(briefs: dict[str, dict]) -> list[list[str]]:
    """DFS cycle detection over depends_on edges within the cluster."""
    WHITE, GREY, BLACK = 0, 1, 2
    color = {bid: WHITE for bid in briefs}
    cycles: list[list[str]] = []

    def visit(bid: str, stack: list[str]) -> None:
        color[bid] = GREY
        stack.append(bid)
        for dep in briefs[bid].get("depends_on", []):
            if dep not in briefs:
                continue
            if color[dep] == GREY:
                cycle = stack[stack.index(dep):] + [dep]
                cycles.append(cycle)
            elif color[dep] == WHITE:
                visit(dep, stack)
        stack.pop()
        color[bid] = BLACK

    for bid in sorted(briefs):
        if color[bid] == WHITE:
            visit(bid, [])
    return cycles


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Check checklist/story coverage, per-brief bidirectional "
        "consistency, structure paths, design anchors, and brief dependencies "
        "for a cluster."
    )
    parser.add_argument("cluster_dir", help="Cluster directory under docs/design/.")
    args = parser.parse_args()

    cluster_dir = Path(args.cluster_dir)
    if not cluster_dir.is_dir():
        print(f"error: {cluster_dir} is not a directory", file=sys.stderr)
        sys.exit(1)

    failures: list[str] = []
    warnings: list[str] = []

    # --- cluster documents -------------------------------------------------
    all_checklist_ids: set[str] = set()
    all_story_ids: set[str] = set()
    has_checklist = False
    has_stories = False

    checklist_json = cluster_dir / "checklist.json"
    if checklist_json.exists():
        has_checklist = True
        data = load_json(checklist_json)
        if data is None:
            sys.exit(1)
        for section in data["sections"]:
            for item in section["items"]:
                all_checklist_ids.add(item["id"])

    stories_json = cluster_dir / "stories.json"
    if stories_json.exists():
        has_stories = True
        data = load_json(stories_json)
        if data is None:
            sys.exit(1)
        for persona in data["personas"]:
            for story in persona["stories"]:
                all_story_ids.add(story["id"])

    structure_paths: list[str] | None = None
    design_json = cluster_dir / "design.json"
    if design_json.exists():
        data = load_json(design_json)
        if data is None:
            sys.exit(1)
        structure_paths = [entry["path"] for entry in data.get("structure", [])]

    adr_ids: set[str] | None = None
    decisions_path = find_decisions_path(cluster_dir)
    if decisions_path is not None:
        data = load_json(decisions_path)
        if data is None:
            sys.exit(1)
        adr_ids = {decision["id"] for decision in data.get("decisions", [])}

    # --- briefs ------------------------------------------------------------
    briefs: dict[str, dict] = {}
    brief_checklist: dict[str, list[str]] = defaultdict(list)
    brief_stories: dict[str, list[str]] = defaultdict(list)

    briefs_dir = cluster_dir / "briefs"
    if briefs_dir.is_dir():
        for brief_path in sorted(briefs_dir.glob("*.json")):
            data = load_json(brief_path)
            if data is None:
                sys.exit(1)
            brief_id = data["id"]
            briefs[brief_id] = data
            for cid in data.get("checklist", []):
                brief_checklist[cid].append(brief_id)
            for sid in data.get("stories", []):
                brief_stories[sid].append(brief_id)

    # --- coverage both directions (cluster <-> briefs) ----------------------
    assigned_checklist = set(brief_checklist)
    assigned_stories = set(brief_stories)

    unassigned_checklist = sorted(
        all_checklist_ids - assigned_checklist, key=_id_sort_key
    )
    unassigned_stories = sorted(all_story_ids - assigned_stories, key=_id_sort_key)

    for cid in unassigned_checklist:
        failures.append(f"checklist item {cid} is in no brief")
    for sid in unassigned_stories:
        failures.append(f"story {sid} is in no brief")

    if has_checklist:
        for cid in sorted(assigned_checklist - all_checklist_ids, key=_id_sort_key):
            failures.append(
                f"unknown checklist id {cid} referenced by "
                f"{', '.join(brief_checklist[cid])}"
            )
    elif assigned_checklist:
        warnings.append("checklist.json missing — unknown-C# check skipped")
    if has_stories:
        for sid in sorted(assigned_stories - all_story_ids, key=_id_sort_key):
            failures.append(
                f"unknown story id {sid} referenced by "
                f"{', '.join(brief_stories[sid])}"
            )
    elif assigned_stories:
        warnings.append("stories.json missing — unknown-S# check skipped")

    multi_checklist = {
        cid: bids for cid, bids in brief_checklist.items() if len(bids) > 1
    }
    multi_stories = {
        sid: bids for sid, bids in brief_stories.items() if len(bids) > 1
    }
    for cid in sorted(multi_checklist, key=_id_sort_key):
        warnings.append(
            f"checklist item {cid} claimed by multiple briefs: "
            f"{', '.join(multi_checklist[cid])} (legal when the split is noted)"
        )
    for sid in sorted(multi_stories, key=_id_sort_key):
        warnings.append(
            f"story {sid} claimed by multiple briefs: "
            f"{', '.join(multi_stories[sid])} (legal when the split is noted)"
        )

    # --- per-brief checks ----------------------------------------------------
    for brief_id, data in sorted(briefs.items()):
        requirements = data.get("requirements", [])

        # Bidirectional: brief-level arrays == union of per-R# arrays.
        union_checklist: set[str] = set()
        union_stories: set[str] = set()
        for req in requirements:
            union_checklist.update(req.get("checklist", []))
            union_stories.update(req.get("stories", []))
        level_checklist = set(data.get("checklist", []))
        level_stories = set(data.get("stories", []))

        for cid in sorted(level_checklist - union_checklist, key=_id_sort_key):
            failures.append(
                f"{brief_id}: brief-level checklist {cid} not covered by any R#"
            )
        for cid in sorted(union_checklist - level_checklist, key=_id_sort_key):
            req_ids = [
                req["id"] for req in requirements if cid in req.get("checklist", [])
            ]
            failures.append(
                f"{brief_id}: checklist {cid} cited by {', '.join(req_ids)} "
                "but missing from the brief-level array"
            )
        for sid in sorted(level_stories - union_stories, key=_id_sort_key):
            failures.append(
                f"{brief_id}: brief-level story {sid} not covered by any R#"
            )
        for sid in sorted(union_stories - level_stories, key=_id_sort_key):
            req_ids = [
                req["id"] for req in requirements if sid in req.get("stories", [])
            ]
            failures.append(
                f"{brief_id}: story {sid} cited by {', '.join(req_ids)} "
                "but missing from the brief-level array"
            )

        # Unknown per-R# ids (brief-level unknowns are caught above).
        for req in requirements:
            if has_checklist:
                for cid in req.get("checklist", []):
                    if cid not in all_checklist_ids:
                        failures.append(
                            f"{brief_id} {req['id']}: unknown checklist id {cid}"
                        )
            if has_stories:
                for sid in req.get("stories", []):
                    if sid not in all_story_ids:
                        failures.append(
                            f"{brief_id} {req['id']}: unknown story id {sid}"
                        )

        # Every R# files path must appear in design.json structure.
        if structure_paths is not None:
            for req in requirements:
                files = req.get("files", {})
                for change in ("create", "modify", "delete"):
                    for path in files.get(change, []):
                        if not path_in_structure(path, structure_paths):
                            failures.append(
                                f"{brief_id} {req['id']}: {change} path "
                                f"{path} not in design.json structure"
                            )
        elif any(
            req.get("files", {}).get(change)
            for req in requirements
            for change in ("create", "modify", "delete")
        ):
            warnings.append(
                f"{brief_id}: design.json missing — structure check skipped"
            )

        # design_anchor ADR ids must exist in the decision ledger.
        anchors = data.get("design_anchor", [])
        if anchors:
            if adr_ids is None:
                warnings.append(
                    f"{brief_id}: decisions.json not found — "
                    "design_anchor check skipped"
                )
            else:
                for adr_id in anchors:
                    if adr_id not in adr_ids:
                        failures.append(
                            f"{brief_id}: design_anchor {adr_id} "
                            "not found in decisions.json"
                        )

        # Dependencies on briefs outside this cluster.
        for dep in data.get("depends_on", []):
            if dep not in briefs:
                warnings.append(
                    f"{brief_id}: depends on {dep}, not in this cluster "
                    "(cross-cluster or unknown)"
                )

    # --- dependency cycles ---------------------------------------------------
    for cycle in detect_cycles(briefs):
        failures.append(f"dependency cycle: {' -> '.join(cycle)}")

    # --- report ----------------------------------------------------------------
    print(f"Cluster: {cluster_dir.name}")
    print(f"  Checklist items: {len(all_checklist_ids)}")
    print(f"  User stories: {len(all_story_ids)}")
    print(f"  Briefs: {len(briefs)}")
    print()

    if briefs:
        print("  Brief dependencies:")
        for brief_id, data in sorted(briefs.items()):
            deps = data.get("depends_on", [])
            dep_str = f" (depends on: {', '.join(deps)})" if deps else ""
            print(f"    {brief_id}: {data['title']}{dep_str}")
        print()

    if warnings:
        print(f"  Warnings ({len(warnings)}):")
        for warning in warnings:
            print(f"    WARN  {warning}")
        print()

    if failures:
        print(f"  Failures ({len(failures)}):")
        for failure in failures:
            print(f"    FAIL  {failure}")
        print()
        print(f"Coverage check FAILED: {len(failures)} failure(s).", file=sys.stderr)
        sys.exit(1)

    print("  Coverage clean: all items covered, briefs consistent.")


if __name__ == "__main__":
    main()
