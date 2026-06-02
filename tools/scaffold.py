#!/usr/bin/env python3
"""Scaffold the Aion workspace from workspace.json + the cluster design.json files.

Reads:
  - workspace.json            (component declarations, deps, external dep versions)
  - docs/design/*/design.json (the per-cluster `structure` maps = the file tree)

Generates a buildable skeleton: the workspace Cargo.toml (with the strict
CLAUDE.md lints and a central [workspace.dependencies] table), one Cargo.toml
per Rust crate (workspace-inherited, internal path deps, external workspace
deps), module stubs with `pub mod` trees derived from the structure, and the
Gleam / Python / TypeScript package manifests.

Idempotent: existing files are left untouched unless --force is given.
Run from the repo root:  python3 tools/scaffold.py [--force] [--dry-run]
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FORCE = "--force" in sys.argv
DRY = "--dry-run" in sys.argv

# Crate-level feature selections for known external deps.
DEP_FEATURES = {
    "serde": ["derive"],
    "tokio": ["full"],
    "uuid": ["v4", "serde"],
    "chrono": ["serde"],
    "tracing-subscriber": ["env-filter"],
}

MANIFEST_BASENAMES = {
    "Cargo.toml", "gleam.toml", "package.json", "pyproject.toml",
    "tsconfig.json", "biome.json",
}

created: list[str] = []
skipped = 0


def write(rel: str, content: str, *, manifest: bool = False) -> None:
    """Write a file, creating parents. Skip if present unless --force."""
    global skipped
    path = ROOT / rel
    if path.is_dir():  # a directory placeholder already occupies this path
        skipped += 1
        return
    if path.exists() and not FORCE:
        skipped += 1
        return
    if DRY:
        created.append(rel + "  (dry-run)")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    created.append(rel)


def normalize(p: str) -> str:
    """Normalize legacy path prefixes (the old 'Wyrd' working name)."""
    if p.startswith("gleam_wyrd/"):
        return "gleam/" + p[len("gleam_wyrd/"):]
    return p


def load_structure() -> dict[str, str]:
    """Union of every cluster design.json `structure` map, path-normalized."""
    out: dict[str, str] = {}
    for design in sorted((ROOT / "docs" / "design").glob("*/design.json")):
        data = json.loads(design.read_text())
        for raw, desc in data.get("structure", {}).items():
            out[normalize(raw)] = desc
    return out


def doc_line(desc: str) -> str:
    """First clause of a structure description, stripped of [BRIEF] tags."""
    d = desc.strip()
    if d.startswith("["):
        end = d.find("]")
        if end != -1:
            d = d[end + 1:].strip()
    return d or "TODO: documentation."


# ---------------------------------------------------------------------------
# Rust module-tree generation
# ---------------------------------------------------------------------------

def rust_module_files(crate_path: str, structure: dict[str, str]) -> dict[str, str]:
    """All .rs files under {crate}/src for this crate, mapped path -> desc."""
    prefix = crate_path + "/src/"
    return {p: d for p, d in structure.items()
            if p.startswith(prefix) and p.endswith(".rs")}


def children_of(dir_rel: str, rs_files: set[str]) -> list[str]:
    """Module names declared directly inside dir_rel (a src-relative dir, '' = src root)."""
    kids: set[str] = set()
    base = dir_rel.rstrip("/")
    for f in rs_files:
        if base:
            if not f.startswith(base + "/"):
                continue
            rest = f[len(base) + 1:]
        else:
            rest = f
        parts = rest.split("/")
        name = parts[0]
        if len(parts) == 1:
            stem = name[:-3]
            if stem not in ("mod", "lib", "main"):
                kids.add(stem)
        else:
            kids.add(name)  # subdirectory module
    return sorted(kids)


def generate_rust_crate(comp: dict, structure: dict[str, str], deps_table: dict) -> None:
    crate_path = comp["path"]
    is_bin = comp["kind"] == "rust-bin"

    rs_map = rust_module_files(crate_path, structure)
    src_files = {p[len(crate_path) + 5:] for p in rs_map}  # strip "{crate}/src/"
    root_name = "lib.rs"
    if is_bin and "lib.rs" not in src_files:
        root_name = "main.rs"
    src_files.add(root_name)

    # Every directory containing .rs files needs a mod.rs owner.
    for f in list(src_files):
        if "/" in f:
            parts = f.split("/")[:-1]
            for i in range(len(parts)):
                d = "/".join(parts[: i + 1])
                modrs = d + "/mod.rs"
                if modrs not in src_files:
                    src_files.add(modrs)
                    rs_map[f"{crate_path}/src/{modrs}"] = "Module declarations."

    rs_set = set(src_files)

    for f in sorted(rs_set):
        rel = f"{crate_path}/src/{f}"
        desc = doc_line(rs_map.get(rel, "Module."))
        if f == root_name:
            kids = children_of("", rs_set)
            kw = "mod" if root_name == "main.rs" else "pub mod"
            body = f"//! {comp['description']}\n\n"
            body += "\n".join(f"{kw} {k};" for k in kids)
            body += "\n" if kids else ""
            if root_name == "main.rs":
                body += "\nfn main() {}\n"
            write(rel, body)
        elif f.endswith("/mod.rs"):
            owner = f[:-7]
            kids = children_of(owner, rs_set)
            body = f"//! {desc}\n\n" + "\n".join(f"pub mod {k};" for k in kids)
            write(rel, body + "\n")
        else:
            write(rel, f"//! {desc}\n")

    write(f"{crate_path}/Cargo.toml", rust_cargo_toml(comp, deps_table), manifest=True)


def rust_cargo_toml(comp: dict, deps_table: dict) -> str:
    lines = ["[package]", f'name = "{comp["id"]}"',
             "version.workspace = true", "edition.workspace = true",
             "rust-version.workspace = true", "", "[lints]", "workspace = true", "",
             "[dependencies]"]
    for dep in comp.get("internal_deps", []):
        lines.append(f'{dep} = {{ path = "../{dep}" }}')
    for dep in comp.get("external_deps", []):
        feats = DEP_FEATURES.get(dep)
        if feats:
            lines.append(f"{dep} = {{ workspace = true, features = {json.dumps(feats)} }}")
        else:
            lines.append(f"{dep} = {{ workspace = true }}")
    if comp.get("build_deps"):
        lines += ["", "[build-dependencies]"]
        for dep in comp["build_deps"]:
            lines.append(f"{dep} = {{ workspace = true }}")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# Workspace Cargo.toml
# ---------------------------------------------------------------------------

def workspace_cargo_toml(spec: dict) -> str:
    members = [c["path"] for c in spec["components"] if c["kind"].startswith("rust")]
    dep_lines = []
    for name, ver in spec["rust"]["dependencies"].items():
        if isinstance(ver, dict):
            inner = ", ".join(f'{k} = "{v}"' for k, v in ver.items())
            dep_lines.append(f"{name} = {{ {inner} }}")
        else:
            dep_lines.append(f'{name} = "{ver}"')
    members_block = ",\n".join(f'    "{m}"' for m in members)
    deps_block = "\n".join(dep_lines)
    return f"""[workspace]
resolver = "2"
members = [
{members_block}
]

[workspace.package]
version = "0.1.0"
edition = "{spec['rust']['edition']}"
rust-version = "{spec['rust']['rust_version']}"
license = "MIT"
repository = "https://github.com/ablative-io/aion"

[workspace.lints.rust]
unsafe_code = "deny"
missing_docs = "warn"

[workspace.lints.clippy]
all = {{ level = "warn", priority = -1 }}
pedantic = {{ level = "warn", priority = -1 }}
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
todo = "warn"

[workspace.dependencies]
{deps_block}
"""


# ---------------------------------------------------------------------------
# Non-Rust packages
# ---------------------------------------------------------------------------

def generate_gleam(comp: dict) -> None:
    name = Path(comp["path"]).name
    deps = "\n".join(
        'gleam_stdlib = ">= 0.34.0 and < 2.0.0"' if d == "gleam_stdlib"
        else f'{d} = ">= 1.0.0 and < 2.0.0"'
        for d in comp.get("external_deps", []))
    write(f"{comp['path']}/gleam.toml",
          f'name = "{name}"\nversion = "0.1.0"\ntarget = "erlang"\n\n'
          f"[dependencies]\n{deps}\n", manifest=True)
    # Source files come from the cluster design.json structure (single source
    # of truth); the generator owns only the manifest.


def generate_python(comp: dict) -> None:
    pkg = comp["id"].replace("-", "_")
    write(f"{comp['path']}/pyproject.toml",
          f'[project]\nname = "{comp["id"]}"\nversion = "0.1.0"\n'
          f'requires-python = ">=3.10"\ndescription = "{comp["description"]}"\n',
          manifest=True)
    # Source files come from the cluster design.json structure.


def generate_typescript(comp: dict) -> None:
    name = comp["id"].replace("aion-", "@aion/")
    write(f"{comp['path']}/package.json",
          json.dumps({"name": name, "version": "0.1.0", "type": "module",
                      "main": "dist/index.js", "types": "dist/index.d.ts",
                      "description": comp["description"]}, indent=2) + "\n",
          manifest=True)
    write(f"{comp['path']}/tsconfig.json",
          json.dumps({"compilerOptions": {"target": "ES2022", "module": "ESNext",
                      "moduleResolution": "bundler", "strict": True,
                      "declaration": True, "outDir": "dist", "rootDir": "src"},
                      "include": ["src"]}, indent=2) + "\n", manifest=True)
    # Source files come from the cluster design.json structure.


def generate_frontend(comp: dict) -> None:
    write(f"{comp['path']}/package.json",
          json.dumps({"name": comp["id"], "version": "0.1.0", "private": True,
                      "type": "module", "description": comp["description"],
                      "scripts": {"dev": "vite", "build": "vite build"}},
                     indent=2) + "\n", manifest=True)


# ---------------------------------------------------------------------------
# Structure-driven leftover stubs (proto, conformance, frontend configs, ...)
# ---------------------------------------------------------------------------

def stub_for(path: str, desc: str) -> str | None:
    base = Path(path).name
    ext = Path(path).suffix
    if base in MANIFEST_BASENAMES or path.startswith("docs/"):
        return None
    if ext == "":  # extensionless structure entries are directory markers
        return None
    d = doc_line(desc)
    if ext == ".rs":
        return None  # handled by the crate generator
    if base == "build.rs":
        return "fn main() {}\n"
    if ext == ".proto":
        return f'syntax = "proto3";\n\npackage aion;\n\n// {d}\n'
    if ext == ".gleam":
        return f"//// {d}\n"
    if ext == ".ts":
        return f"// {d}\n"
    if ext == ".py":
        return f'"""{d}"""\n'
    if ext == ".json":
        return "{}\n"
    if ext == ".md":
        return f"# {d}\n"
    if ext == ".toml":
        return f"# {d}\n"
    if ext in (".css", ".html"):
        return f"/* {d} */\n"
    return ""


def main() -> None:
    spec = json.loads((ROOT / "workspace.json").read_text())
    structure = load_structure()
    deps_table = spec["rust"]["dependencies"]

    write("Cargo.toml", workspace_cargo_toml(spec), manifest=True)

    for comp in spec["components"]:
        kind = comp["kind"]
        if kind.startswith("rust"):
            generate_rust_crate(comp, structure, deps_table)
        elif kind == "gleam":
            generate_gleam(comp)
        elif kind == "python":
            generate_python(comp)
        elif kind == "typescript":
            generate_typescript(comp)
        elif kind == "frontend":
            generate_frontend(comp)

    for path, desc in sorted(structure.items()):
        content = stub_for(path, desc)
        if content is not None:
            write(path, content)

    verb = "Would create" if DRY else "Created"
    print(f"{verb} {len(created)} file(s); skipped {skipped} existing.")


if __name__ == "__main__":
    main()
