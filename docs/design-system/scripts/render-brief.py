#!/usr/bin/env python3
"""Render a brief JSON file to Markdown.

Usage:
  render-brief.py <brief.json> [output.md] [--cluster-dir DIR] [--no-resolve]

Resolves checklist (C-number) and story (S-number) references to their
actual text when the cluster directory is available (auto-detected as
the parent of briefs/ unless --cluster-dir is given), and design_anchor
ADR ids against the project decision ledger (decisions.json, found by
searching upward from the cluster directory).

When the pipeline has enriched the brief in place, the per-requirement
scout/dev/review blocks and the brief-level execution block are rendered
in clearly-marked sections, so an enriched brief reads as
spec-then-record.
"""
import argparse
import json
import sys
from pathlib import Path


def build_checklist_lookup(data: dict) -> dict[str, str]:
    lookup = {}
    for section in data.get("sections", []):
        for item in section.get("items", []):
            lookup[item["id"]] = item["text"]
    return lookup


def build_stories_lookup(data: dict) -> dict[str, dict]:
    lookup = {}
    for persona in data.get("personas", []):
        for story in persona.get("stories", []):
            lookup[story["id"]] = {
                "text": story["text"],
                "persona": persona["name"],
                "role": persona["role"],
            }
    return lookup


def build_adr_lookup(data: dict) -> dict[str, str]:
    """Map ADR id -> 'ADR-003 — title — decision'."""
    lookup = {}
    for decision in data.get("decisions", []):
        lookup[decision["id"]] = (
            f"{decision['id']} — {decision['title']} — {decision['decision']}"
        )
    return lookup


def _resolve_checklist_ids(
    ids: list[str], lookup: dict[str, str] | None
) -> list[str]:
    if not lookup:
        return list(ids)
    resolved = []
    for cid in ids:
        text = lookup.get(cid)
        resolved.append(f"{cid} — {text}" if text else cid)
    return resolved


def _resolve_story_ids(
    ids: list[str], lookup: dict[str, dict] | None
) -> list[str]:
    if not lookup:
        return list(ids)
    resolved = []
    for sid in ids:
        entry = lookup.get(sid)
        if entry:
            resolved.append(
                f"{sid} ({entry['persona']}, {entry['role']}) — {entry['text']}"
            )
        else:
            resolved.append(sid)
    return resolved


def _resolve_adr_ids(ids: list[str], lookup: dict[str, str] | None) -> list[str]:
    if not lookup:
        return list(ids)
    return [lookup.get(adr_id, adr_id) for adr_id in ids]


def _mark(flag: bool) -> str:
    return "x" if flag else " "


def _render_scout(scout: dict, lines: list[str]) -> None:
    lines.append("**Scout (recorded):**")
    lines.append("")
    if scout.get("files"):
        lines.append("- Files:")
        for entry in scout["files"]:
            lines.append(f"  - {entry}")
    if scout.get("context"):
        lines.append("- Context:")
        for entry in scout["context"]:
            lines.append(f"  - {entry}")
    lines.append(f"- Approach: {scout['approach']}")
    if scout.get("notes"):
        lines.append(f"- Notes: {scout['notes']}")
    lines.append("")


def _render_dev(
    dev: dict,
    lines: list[str],
    checklist_lookup: dict[str, str] | None,
    stories_lookup: dict[str, dict] | None,
) -> None:
    lines.append("**Dev (recorded):**")
    lines.append("")
    lines.append(f"- Status: {dev['status']}")
    lines.append(f"- How: {dev['how']}")
    deviation = dev.get("deviation", "")
    lines.append(f"- Deviation: {deviation if deviation else '(none)'}")
    if dev.get("files_changed"):
        lines.append("- Files changed:")
        for change in dev["files_changed"]:
            note = f" — {change['note']}" if change.get("note") else ""
            lines.append(f"  - {change['change']}: `{change['path']}`{note}")
    if dev.get("checklist"):
        lines.append("- Checklist delivery:")
        for claim in dev["checklist"]:
            (resolved,) = _resolve_checklist_ids([claim["id"]], checklist_lookup)
            note = f" — {claim['note']}" if claim.get("note") else ""
            lines.append(f"  - [{_mark(claim['done'])}] {resolved}{note}")
    if dev.get("stories"):
        lines.append("- Story delivery:")
        for claim in dev["stories"]:
            (resolved,) = _resolve_story_ids([claim["id"]], stories_lookup)
            note = f" — {claim['note']}" if claim.get("note") else ""
            lines.append(f"  - [{_mark(claim['satisfied'])}] {resolved}{note}")
    lines.append("")


def _render_review(review: dict, lines: list[str]) -> None:
    lines.append("**Review (recorded):**")
    lines.append("")
    lines.append(f"- Alignment: {review['alignment']}")
    if review.get("acceptance"):
        lines.append("- Acceptance verdicts:")
        for verdict in review["acceptance"]:
            evidence = (
                f" — {verdict['evidence']}" if verdict.get("evidence") else ""
            )
            lines.append(
                f"  - [{_mark(verdict['met'])}] {verdict['criterion']}{evidence}"
            )
    if review.get("checklist"):
        lines.append(f"- Checklist verified: {', '.join(review['checklist'])}")
    if review.get("stories"):
        lines.append(f"- Stories verified: {', '.join(review['stories'])}")
    if review.get("issues"):
        lines.append("- Issues:")
        for issue in review["issues"]:
            lines.append(f"  - {issue}")
    if review.get("fixes"):
        lines.append("- Fixes:")
        for fix in review["fixes"]:
            lines.append(f"  - {fix}")
    lines.append("")


def _render_execution(execution: dict, lines: list[str]) -> None:
    lines.append("## Execution Record")
    lines.append("")
    lines.append("> Appended by the pipeline — the run-level record. The gate")
    lines.append("> block is what the workflow measured; the attestation block")
    lines.append("> is what the dev agent believed.")
    lines.append("")
    lines.append(f"- **Status:** {execution['status']}")
    lines.append(f"- **Workflow:** {execution['workflow_id']}")
    lines.append(f"- **Branch:** {execution['branch']}")
    lines.append(f"- **Session:** {execution['session_id']}")
    gate = execution["gate"]
    gate_parts = [
        f"fmt {'pass' if gate['fmt'] else 'FAIL'}",
        f"clippy {'pass' if gate['clippy'] else 'FAIL'}",
        f"tests {'pass' if gate['tests'] else 'FAIL'}",
        f"fix rounds: {gate['fix_rounds']}",
    ]
    lines.append(f"- **Gate (measured):** {', '.join(gate_parts)}")
    attestation = execution["attestation"]
    att_parts = [
        f"{key} {'yes' if attestation[key] else 'NO'}"
        for key in ("no_panics", "no_unsafe", "boundaries_respected", "tests_pass")
    ]
    lines.append(f"- **Attestation (believed):** {', '.join(att_parts)}")
    lines.append(f"- **Review verdict:** {execution['review_verdict']}")
    landed = execution.get("landed_commit", "")
    merged = execution.get("merged_into", "")
    if landed or merged:
        arrow = f" -> {merged}" if merged else ""
        lines.append(f"- **Landed commit:** {landed or '(pending)'}{arrow}")
    completed = execution.get("completed_at", "")
    if completed:
        lines.append(f"- **Completed:** {completed}")
    lines.append("")


def render(
    data: dict,
    checklist_lookup: dict[str, str] | None = None,
    stories_lookup: dict[str, dict] | None = None,
    adr_lookup: dict[str, str] | None = None,
) -> str:
    lines = []

    # YAML frontmatter
    lines.append("---")
    lines.append("type: brief")
    lines.append(f"id: {data['id']}")
    lines.append(f"cluster: {data['cluster']}")
    lines.append(f"title: {data['title']}")
    lines.append("---")
    lines.append("")

    # Header
    lines.append(f"# {data['id']}: {data['title']}")
    lines.append("")

    # Metadata block
    lines.append(f"> **Cluster:** {data['cluster']}")
    if data.get("depends_on"):
        lines.append(f"> **Depends on:** {', '.join(data['depends_on'])}")
    if data.get("blocked_by"):
        lines.append(f"> **Blocked by:** {', '.join(data['blocked_by'])}")

    if data.get("design_anchor"):
        resolved = _resolve_adr_ids(data["design_anchor"], adr_lookup)
        if adr_lookup:
            lines.append("> **Design anchor:**")
            for item in resolved:
                lines.append(f"> - {item}")
        else:
            lines.append(f"> **Design anchor:** {', '.join(resolved)}")

    if data.get("checklist"):
        resolved = _resolve_checklist_ids(data["checklist"], checklist_lookup)
        if checklist_lookup:
            lines.append("> **Checklist:**")
            for item in resolved:
                lines.append(f"> - {item}")
        else:
            lines.append(f"> **Checklist:** {', '.join(resolved)}")

    if data.get("stories"):
        resolved = _resolve_story_ids(data["stories"], stories_lookup)
        if stories_lookup:
            lines.append("> **Stories:**")
            for item in resolved:
                lines.append(f"> - {item}")
        else:
            lines.append(f"> **Stories:** {', '.join(resolved)}")

    lines.append("")

    # Purpose
    lines.append("## Purpose")
    lines.append("")
    lines.append(data["purpose"])
    lines.append("")

    # Task
    lines.append("## Task")
    lines.append("")
    lines.append(data["task"])
    lines.append("")

    # Requirements — spec first, then any recorded execution blocks
    lines.append("## Requirements")
    lines.append("")

    for req in data["requirements"]:
        lines.append(f"### {req['id']}: {req['title']}")
        lines.append("")
        lines.append(req["spec"])
        lines.append("")

        lines.append("**Acceptance:**")
        for criterion in req["acceptance"]:
            lines.append(f"- {criterion}")
        lines.append("")

        files = req.get("files", {})
        if any(files.get(k) for k in ("create", "modify", "delete")):
            lines.append("**Files:**")
            for path in files.get("create", []):
                lines.append(f"- create: {path}")
            for path in files.get("modify", []):
                lines.append(f"- modify: {path}")
            for path in files.get("delete", []):
                lines.append(f"- delete: {path}")
            lines.append("")

        if req.get("checklist"):
            resolved = _resolve_checklist_ids(req["checklist"], checklist_lookup)
            lines.append("**Checklist:**")
            for item in resolved:
                lines.append(f"- {item}")
            lines.append("")

        if req.get("stories"):
            resolved = _resolve_story_ids(req["stories"], stories_lookup)
            lines.append("**Stories:**")
            for item in resolved:
                lines.append(f"- {item}")
            lines.append("")

        # Enrichment blocks (appended by the pipeline) — the record.
        if any(key in req for key in ("scout", "dev", "review")):
            lines.append(f"#### {req['id']} — Execution record")
            lines.append("")
            if "scout" in req:
                _render_scout(req["scout"], lines)
            if "dev" in req:
                _render_dev(req["dev"], lines, checklist_lookup, stories_lookup)
            if "review" in req:
                _render_review(req["review"], lines)

    # Boundaries
    if data.get("boundaries"):
        lines.append("## Boundaries")
        lines.append("")
        for boundary in data["boundaries"]:
            lines.append(f"- {boundary}")
        lines.append("")

    # Verification
    if data.get("verification"):
        lines.append("## Verification")
        lines.append("")
        for step in data["verification"]:
            lines.append(f"- {step}")
        lines.append("")

    # Brief-level execution record (appended by the pipeline) — last.
    if "execution" in data:
        _render_execution(data["execution"], lines)

    return "\n".join(lines)


def find_cluster_dir(brief_path: Path) -> Path | None:
    """Auto-detect cluster directory from brief file location.

    Expected layout: docs/design/{cluster}/briefs/{brief}.json
    Cluster dir is the parent of briefs/.
    """
    if brief_path.parent.name == "briefs":
        candidate = brief_path.parent.parent
        if (candidate / "checklist.json").exists() or (
            candidate / "stories.json"
        ).exists():
            return candidate
    return None


def find_decisions_path(start: Path) -> Path | None:
    """Search upward from start for the project decision ledger."""
    current = start.resolve()
    for candidate in (current, *current.parents):
        ledger = candidate / "decisions.json"
        if ledger.is_file():
            return ledger
    return None


def load_lookups(
    cluster_dir: Path,
) -> tuple[
    dict[str, str] | None, dict[str, dict] | None, dict[str, str] | None
]:
    checklist_lookup = None
    stories_lookup = None
    adr_lookup = None

    checklist_path = cluster_dir / "checklist.json"
    if checklist_path.exists():
        with open(checklist_path) as handle:
            checklist_lookup = build_checklist_lookup(json.load(handle))

    stories_path = cluster_dir / "stories.json"
    if stories_path.exists():
        with open(stories_path) as handle:
            stories_lookup = build_stories_lookup(json.load(handle))

    decisions_path = find_decisions_path(cluster_dir)
    if decisions_path is not None:
        with open(decisions_path) as handle:
            adr_lookup = build_adr_lookup(json.load(handle))

    return checklist_lookup, stories_lookup, adr_lookup


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Render a brief JSON file to Markdown with resolved "
        "C#/S#/ADR# references and any recorded execution blocks."
    )
    parser.add_argument("brief", help="Path to brief JSON file.")
    parser.add_argument("output", nargs="?", help="Output path (stdout if omitted).")
    parser.add_argument(
        "--cluster-dir",
        help="Cluster directory containing checklist.json and stories.json. "
        "Auto-detected from brief path if omitted.",
    )
    parser.add_argument(
        "--no-resolve",
        action="store_true",
        help="Skip reference resolution even if cluster files are available.",
    )
    args = parser.parse_args()

    src = Path(args.brief)
    try:
        with open(src) as handle:
            data = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"error: cannot read {src}: {exc}", file=sys.stderr)
        sys.exit(1)

    checklist_lookup = None
    stories_lookup = None
    adr_lookup = None

    if not args.no_resolve:
        cluster_dir = (
            Path(args.cluster_dir) if args.cluster_dir else find_cluster_dir(src)
        )
        if cluster_dir:
            checklist_lookup, stories_lookup, adr_lookup = load_lookups(cluster_dir)

    md = render(data, checklist_lookup, stories_lookup, adr_lookup)

    if args.output:
        Path(args.output).write_text(md)
        print(f"Rendered to {args.output}", file=sys.stderr)
    else:
        print(md)


if __name__ == "__main__":
    main()
