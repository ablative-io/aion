#!/usr/bin/env python3
"""Verify that every roadmap status claim is backed by its artifact.

Usage:
  check-roadmap.py <design-dir>

<design-dir> is the directory holding roadmap.json, decisions.json, and
the cluster subdirectories (e.g. docs/design).

Per the status table in guides/ROADMAP.md (FAIL = exit 1):
  - designed:   non-empty links.cluster (and <design-dir>/<cluster>/design.json
                exists) OR non-empty links.decisions (and those ADR ids exist
                in decisions.json)
  - briefed:    the designed checks, plus non-empty links.briefs with every
                brief file present in the cluster's briefs/
  - dispatched: the briefed checks, plus every linked brief carrying an
                execution block with a workflow id
  - landed:     non-empty links.commits
  - dropped:    non-empty notes (the reason)

Also (FAIL): depends_on ids must exist in the ledger; duplicate item ids
are an error. Warns (not a failure) when a non-terminal item depends on
a dropped item.
"""
import argparse
import json
import sys
from pathlib import Path

TERMINAL_STATUSES = {"landed", "dropped"}


def load_json(path: Path):
    try:
        with open(path) as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"error: cannot read {path}: {exc}", file=sys.stderr)
        sys.exit(1)


def check_designed(item: dict, design_dir: Path, adr_ids: set[str] | None,
                   violations: list[str]) -> None:
    """designed = a linked cluster whose design.json exists, and/or linked
    ADRs that exist in the decision ledger."""
    item_id = item["id"]
    links = item["links"]
    cluster = links.get("cluster", "")
    decisions = links.get("decisions", [])

    if not cluster and not decisions:
        violations.append(
            f"{item_id}: status requires links.cluster or links.decisions, "
            "both are empty"
        )
        return

    if cluster:
        design_json = design_dir / cluster / "design.json"
        if not design_json.is_file():
            violations.append(
                f"{item_id}: links.cluster is '{cluster}' but "
                f"{design_json} does not exist"
            )
    if decisions:
        if adr_ids is None:
            violations.append(
                f"{item_id}: links.decisions set but decisions.json "
                "not found in the design dir"
            )
        else:
            for adr_id in decisions:
                if adr_id not in adr_ids:
                    violations.append(
                        f"{item_id}: links.decisions cites {adr_id}, "
                        "not in decisions.json"
                    )


def check_briefed(item: dict, design_dir: Path, violations: list[str]
                  ) -> list[Path]:
    """briefed = brief links present and every brief file exists.
    Returns the paths of the brief files that do exist."""
    item_id = item["id"]
    links = item["links"]
    cluster = links.get("cluster", "")
    briefs = links.get("briefs", [])
    found: list[Path] = []

    if not briefs:
        violations.append(
            f"{item_id}: status requires non-empty links.briefs"
        )
        return found
    if not cluster:
        violations.append(
            f"{item_id}: links.briefs set but links.cluster is empty — "
            "briefs cannot be located"
        )
        return found

    briefs_dir = design_dir / cluster / "briefs"
    for brief_id in briefs:
        brief_path = briefs_dir / f"{brief_id}.json"
        if brief_path.is_file():
            found.append(brief_path)
        else:
            violations.append(
                f"{item_id}: linked brief {brief_id} has no file at {brief_path}"
            )
    return found


def check_dispatched(item: dict, brief_paths: list[Path],
                     violations: list[str]) -> None:
    """dispatched = every linked brief carries an execution block."""
    item_id = item["id"]
    for brief_path in brief_paths:
        try:
            with open(brief_path) as handle:
                brief = json.load(handle)
        except (OSError, json.JSONDecodeError) as exc:
            violations.append(f"{item_id}: cannot read {brief_path}: {exc}")
            continue
        execution = brief.get("execution")
        if not isinstance(execution, dict):
            violations.append(
                f"{item_id}: brief {brief.get('id', brief_path.stem)} has no "
                "execution block — dispatched requires a workflow run"
            )
        elif not execution.get("workflow_id"):
            violations.append(
                f"{item_id}: brief {brief.get('id', brief_path.stem)} execution "
                "block has empty workflow_id"
            )


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Verify roadmap status claims against their artifacts "
        "(links, files, execution records)."
    )
    parser.add_argument(
        "design_dir",
        help="Directory holding roadmap.json, decisions.json, and the "
        "cluster subdirectories (e.g. docs/design).",
    )
    args = parser.parse_args()

    design_dir = Path(args.design_dir)
    roadmap_path = design_dir / "roadmap.json"
    if not roadmap_path.is_file():
        print(f"error: {roadmap_path} does not exist", file=sys.stderr)
        sys.exit(1)
    roadmap = load_json(roadmap_path)

    adr_ids: set[str] | None = None
    decisions_path = design_dir / "decisions.json"
    if decisions_path.is_file():
        ledger = load_json(decisions_path)
        adr_ids = {decision["id"] for decision in ledger.get("decisions", [])}

    items = roadmap.get("items", [])
    violations: list[str] = []
    warnings: list[str] = []

    # Duplicate ids are an error.
    seen: dict[str, int] = {}
    for item in items:
        seen[item["id"]] = seen.get(item["id"], 0) + 1
    for item_id, count in sorted(seen.items()):
        if count > 1:
            violations.append(f"{item_id}: duplicate id ({count} entries)")

    by_id = {item["id"]: item for item in items}
    status_counts: dict[str, int] = {}

    for item in items:
        item_id = item["id"]
        status = item["status"]
        status_counts[status] = status_counts.get(status, 0) + 1

        # Status-specific artifact checks (cumulative through dispatched).
        if status in ("designed", "briefed", "dispatched"):
            check_designed(item, design_dir, adr_ids, violations)
        brief_paths: list[Path] = []
        if status in ("briefed", "dispatched"):
            brief_paths = check_briefed(item, design_dir, violations)
        if status == "dispatched":
            check_dispatched(item, brief_paths, violations)
        if status == "landed" and not item["links"].get("commits"):
            violations.append(
                f"{item_id}: status landed requires non-empty links.commits"
            )
        if status == "dropped" and not item.get("notes"):
            violations.append(
                f"{item_id}: status dropped requires the reason in notes"
            )

        # Dependency hygiene.
        for dep in item.get("depends_on", []):
            dep_item = by_id.get(dep)
            if dep_item is None:
                violations.append(
                    f"{item_id}: depends_on cites {dep}, not in the ledger"
                )
            elif (
                status not in TERMINAL_STATUSES
                and dep_item["status"] == "dropped"
            ):
                warnings.append(
                    f"{item_id} [{status}] depends on dropped item {dep}"
                )

    # --- report ---------------------------------------------------------------
    print(f"Roadmap: {roadmap.get('project', '?')} "
          f"(updated {roadmap.get('updated', '?')})")
    summary = ", ".join(
        f"{status} {count}" for status, count in sorted(status_counts.items())
    )
    print(f"  Items: {len(items)}" + (f" ({summary})" if summary else ""))
    print()

    if warnings:
        print(f"  Warnings ({len(warnings)}):")
        for warning in warnings:
            print(f"    WARN  {warning}")
        print()

    if violations:
        print(f"  Violations ({len(violations)}):")
        for violation in violations:
            print(f"    FAIL  {violation}")
        print()
        print(
            f"Roadmap check FAILED: {len(violations)} violation(s).",
            file=sys.stderr,
        )
        sys.exit(1)

    print("  All status claims are backed by their artifacts.")


if __name__ == "__main__":
    main()
