#!/usr/bin/env python3
"""Render all JSON documents in a cluster directory to Markdown.

Usage:
  render-cluster.py <cluster-dir>

Expects (any subset of):
  <cluster-dir>/design.json
  <cluster-dir>/checklist.json
  <cluster-dir>/stories.json
  <cluster-dir>/briefs/*.json

Produces, next to each JSON source:
  <cluster-dir>/DESIGN.md
  <cluster-dir>/CHECKLIST.md
  <cluster-dir>/USER-STORIES.md
  <cluster-dir>/briefs/*.md  (C#/S#/ADR# resolved, execution records included)

DESIGN.md sections follow schema order: intention, problem, solution,
principles, decisions (as ADR refs), goals, non_goals, structure (as a
path|note|brief table), inventory, constraints.
"""
import argparse
import json
import sys
from importlib import import_module
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

_brief_mod = import_module("render-brief")
render_brief = _brief_mod.render
build_checklist_lookup = _brief_mod.build_checklist_lookup
build_stories_lookup = _brief_mod.build_stories_lookup
build_adr_lookup = _brief_mod.build_adr_lookup
find_decisions_path = _brief_mod.find_decisions_path


def render_checklist(data: dict) -> str:
    lines = [f"# {data['cluster'].title()} — Checklist", ""]
    for section in data["sections"]:
        lines.append(f"## {section['name']}")
        lines.append("")
        for item in section["items"]:
            check = "x" if item.get("done", False) else " "
            lines.append(f"- [{check}] **{item['id']}** — {item['text']}")
        lines.append("")
    return "\n".join(lines)


def render_stories(data: dict) -> str:
    lines = [f"# {data['cluster'].title()} — User Stories", ""]
    for persona in data["personas"]:
        lines.append(f"## {persona['name']} — {persona['role']}")
        lines.append("")
        for story in persona["stories"]:
            lines.append(f"**{story['id']}.** {story['text']}")
            lines.append("")
    return "\n".join(lines)


def _table_cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def render_design(data: dict, adr_lookup: dict[str, str] | None) -> str:
    lines = []

    lines.append("---")
    lines.append("type: design")
    lines.append(f"cluster: {data['cluster']}")
    lines.append(f"title: {data['title']}")
    lines.append("---")
    lines.append("")
    lines.append(f"# {data['title']}")
    lines.append("")
    lines.append(f"> **Cluster:** {data['cluster']}")
    lines.append("")

    lines.append("## Intention")
    lines.append("")
    lines.append(data["intention"])
    lines.append("")

    lines.append("## Problem")
    lines.append("")
    lines.append(data["problem"])
    lines.append("")

    lines.append("## Solution")
    lines.append("")
    lines.append(data["solution"])
    lines.append("")

    if data.get("principles"):
        lines.append("## Principles")
        lines.append("")
        for principle in data["principles"]:
            lines.append(f"- **{principle['id']}** — {principle['text']}")
        lines.append("")

    if data.get("decisions"):
        lines.append("## Decisions")
        lines.append("")
        for adr_id in data["decisions"]:
            if adr_lookup and adr_id in adr_lookup:
                lines.append(f"- {adr_lookup[adr_id]}")
            else:
                lines.append(f"- {adr_id}")
        lines.append("")

    if data.get("goals"):
        lines.append("## Goals")
        lines.append("")
        for goal in data["goals"]:
            lines.append(f"- {goal}")
        lines.append("")

    if data.get("non_goals"):
        lines.append("## Non-Goals")
        lines.append("")
        for non_goal in data["non_goals"]:
            reason = f" — {non_goal['reason']}" if non_goal.get("reason") else ""
            lines.append(f"- {non_goal['text']}{reason}")
        lines.append("")

    if data.get("structure"):
        lines.append("## Structure")
        lines.append("")
        lines.append("| Path | Note | Brief |")
        lines.append("|------|------|-------|")
        for entry in data["structure"]:
            path = _table_cell(entry["path"])
            note = _table_cell(entry["note"])
            brief = _table_cell(entry["brief"]) if entry.get("brief") else ""
            lines.append(f"| `{path}` | {note} | {brief} |")
        lines.append("")

    if data.get("inventory"):
        lines.append("## Inventory")
        lines.append("")
        for entry in data["inventory"]:
            lines.append(f"- `{entry['path']}` — {entry['note']}")
        lines.append("")

    if data.get("constraints"):
        lines.append("## Constraints")
        lines.append("")
        for constraint in data["constraints"]:
            lines.append(f"- **{constraint['id']}** — {constraint['text']}")
        lines.append("")

    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Render a cluster's design/checklist/stories/briefs "
        "JSON documents to Markdown, next to their sources."
    )
    parser.add_argument("cluster_dir", help="Cluster directory under docs/design/.")
    args = parser.parse_args()

    cluster_dir = Path(args.cluster_dir)
    if not cluster_dir.is_dir():
        print(f"error: {cluster_dir} is not a directory", file=sys.stderr)
        sys.exit(1)

    rendered = 0
    checklist_lookup = None
    stories_lookup = None

    adr_lookup = None
    decisions_path = find_decisions_path(cluster_dir)
    if decisions_path is not None:
        with open(decisions_path) as handle:
            adr_lookup = build_adr_lookup(json.load(handle))

    design_json = cluster_dir / "design.json"
    if design_json.exists():
        with open(design_json) as handle:
            design_data = json.load(handle)
        out = cluster_dir / "DESIGN.md"
        out.write_text(render_design(design_data, adr_lookup))
        print(f"  Rendered {out}")
        rendered += 1

    checklist_json = cluster_dir / "checklist.json"
    if checklist_json.exists():
        with open(checklist_json) as handle:
            checklist_data = json.load(handle)
        checklist_lookup = build_checklist_lookup(checklist_data)
        out = cluster_dir / "CHECKLIST.md"
        out.write_text(render_checklist(checklist_data))
        print(f"  Rendered {out}")
        rendered += 1

    stories_json = cluster_dir / "stories.json"
    if stories_json.exists():
        with open(stories_json) as handle:
            stories_data = json.load(handle)
        stories_lookup = build_stories_lookup(stories_data)
        out = cluster_dir / "USER-STORIES.md"
        out.write_text(render_stories(stories_data))
        print(f"  Rendered {out}")
        rendered += 1

    briefs_dir = cluster_dir / "briefs"
    if briefs_dir.is_dir():
        for brief_json in sorted(briefs_dir.glob("*.json")):
            with open(brief_json) as handle:
                data = json.load(handle)
            out = brief_json.with_suffix(".md")
            out.write_text(
                render_brief(data, checklist_lookup, stories_lookup, adr_lookup)
            )
            print(f"  Rendered {out}")
            rendered += 1

    if rendered == 0:
        print(f"  No JSON documents found in {cluster_dir}", file=sys.stderr)
        sys.exit(1)

    print(f"Done. {rendered} file(s) rendered.")


if __name__ == "__main__":
    main()
