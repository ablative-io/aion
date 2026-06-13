#!/usr/bin/env python3
"""Render the project ledgers (roadmap.json, decisions.json) to Markdown.

Usage:
  render-ledgers.py <design-dir>

Expects <design-dir>/roadmap.json and/or <design-dir>/decisions.json and
writes ROADMAP.md / DECISIONS.md next to them.

ROADMAP.md groups items by status in lifecycle order (dispatched,
briefed, designed, idea, landed, dropped). Each item shows id, title,
kind, summary, the provenance quote (as an attributed blockquote when
non-empty), links, and depends_on.

DECISIONS.md lists decided entries first, then proposed, then
superseded. Each shows id, title, scope, date, decided_by, context,
decision, the quote (attributed blockquote when non-empty),
consequences, and supersession links.
"""
import argparse
import json
import sys
from pathlib import Path

ROADMAP_STATUS_ORDER = ["dispatched", "briefed", "designed", "idea", "landed", "dropped"]
DECISION_STATUS_ORDER = ["decided", "proposed", "superseded"]


def _blockquote(quote: str, attribution: str, lines: list[str]) -> None:
    for quote_line in quote.splitlines() or [""]:
        lines.append(f"> {quote_line}")
    lines.append(f"> — {attribution}")
    lines.append("")


def _render_links(links: dict, lines: list[str]) -> None:
    parts = []
    if links.get("cluster"):
        parts.append(f"cluster `{links['cluster']}`")
    if links.get("decisions"):
        parts.append(f"decisions {', '.join(links['decisions'])}")
    if links.get("briefs"):
        parts.append(f"briefs {', '.join(links['briefs'])}")
    if links.get("commits"):
        parts.append(f"commits {', '.join(links['commits'])}")
    lines.append(f"- **Links:** {'; '.join(parts) if parts else '(none)'}")


def render_roadmap(data: dict) -> str:
    lines = [f"# {data['project']} — Roadmap", ""]
    lines.append(f"_Updated: {data['updated']}_")
    lines.append("")

    by_status: dict[str, list[dict]] = {status: [] for status in ROADMAP_STATUS_ORDER}
    unknown_status: list[dict] = []
    for item in data["items"]:
        bucket = by_status.get(item["status"])
        if bucket is None:
            unknown_status.append(item)
        else:
            bucket.append(item)

    for status in ROADMAP_STATUS_ORDER:
        items = by_status[status]
        if not items:
            continue
        lines.append(f"## {status.title()} ({len(items)})")
        lines.append("")
        for item in items:
            lines.append(f"### {item['id']} — {item['title']}")
            lines.append("")
            lines.append(f"- **Kind:** {item['kind']}")
            lines.append("")
            lines.append(item["summary"])
            lines.append("")
            provenance = item["provenance"]
            if provenance.get("quote"):
                _blockquote(
                    provenance["quote"],
                    f"{provenance['requested_by']}, {provenance['date']}",
                    lines,
                )
            _render_links(item["links"], lines)
            if item.get("depends_on"):
                lines.append(f"- **Depends on:** {', '.join(item['depends_on'])}")
            if item.get("notes"):
                lines.append(f"- **Notes:** {item['notes']}")
            lines.append("")

    if unknown_status:
        lines.append("## Unknown Status")
        lines.append("")
        for item in unknown_status:
            lines.append(f"- {item['id']} — {item['title']} ({item['status']})")
        lines.append("")

    return "\n".join(lines)


def render_decisions(data: dict) -> str:
    lines = [f"# {data['project']} — Decisions", ""]
    lines.append(f"_Updated: {data['updated']}_")
    lines.append("")

    by_status: dict[str, list[dict]] = {status: [] for status in DECISION_STATUS_ORDER}
    unknown_status: list[dict] = []
    for decision in data["decisions"]:
        bucket = by_status.get(decision["status"])
        if bucket is None:
            unknown_status.append(decision)
        else:
            bucket.append(decision)

    for status in DECISION_STATUS_ORDER:
        decisions = by_status[status]
        if not decisions:
            continue
        lines.append(f"## {status.title()} ({len(decisions)})")
        lines.append("")
        for decision in decisions:
            lines.append(f"### {decision['id']} — {decision['title']}")
            lines.append("")
            meta = [f"**Scope:** {decision['scope']}"]
            if decision.get("date"):
                meta.append(f"**Date:** {decision['date']}")
            if decision.get("decided_by"):
                meta.append(f"**Decided by:** {decision['decided_by']}")
            lines.append("- " + " · ".join(meta))
            lines.append("")
            lines.append(f"**Context.** {decision['context']}")
            lines.append("")
            lines.append(f"**Decision.** {decision['decision']}")
            lines.append("")
            if decision.get("quote"):
                attribution = decision["decided_by"] or "(undecided)"
                if decision.get("date"):
                    attribution = f"{attribution}, {decision['date']}"
                _blockquote(decision["quote"], attribution, lines)
            if decision.get("consequences"):
                lines.append("**Consequences:**")
                for consequence in decision["consequences"]:
                    lines.append(f"- {consequence}")
                lines.append("")
            supersession = []
            if decision.get("supersedes"):
                supersession.append(f"supersedes {', '.join(decision['supersedes'])}")
            if decision.get("superseded_by"):
                supersession.append(f"superseded by {decision['superseded_by']}")
            if supersession:
                lines.append(f"- **Supersession:** {'; '.join(supersession)}")
                lines.append("")

    if unknown_status:
        lines.append("## Unknown Status")
        lines.append("")
        for decision in unknown_status:
            lines.append(
                f"- {decision['id']} — {decision['title']} ({decision['status']})"
            )
        lines.append("")

    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Render roadmap.json and decisions.json to ROADMAP.md "
        "and DECISIONS.md in the same directory."
    )
    parser.add_argument(
        "design_dir",
        help="Directory holding roadmap.json and/or decisions.json "
        "(e.g. docs/design).",
    )
    args = parser.parse_args()

    design_dir = Path(args.design_dir)
    if not design_dir.is_dir():
        print(f"error: {design_dir} is not a directory", file=sys.stderr)
        sys.exit(1)

    rendered = 0

    roadmap_json = design_dir / "roadmap.json"
    if roadmap_json.exists():
        with open(roadmap_json) as handle:
            roadmap_data = json.load(handle)
        out = design_dir / "ROADMAP.md"
        out.write_text(render_roadmap(roadmap_data))
        print(f"  Rendered {out}")
        rendered += 1

    decisions_json = design_dir / "decisions.json"
    if decisions_json.exists():
        with open(decisions_json) as handle:
            decisions_data = json.load(handle)
        out = design_dir / "DECISIONS.md"
        out.write_text(render_decisions(decisions_data))
        print(f"  Rendered {out}")
        rendered += 1

    if rendered == 0:
        print(
            f"  Neither roadmap.json nor decisions.json found in {design_dir}",
            file=sys.stderr,
        )
        sys.exit(1)

    print(f"Done. {rendered} ledger(s) rendered.")


if __name__ == "__main__":
    main()
