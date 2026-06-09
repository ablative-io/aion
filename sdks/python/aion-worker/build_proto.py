"""Hatch build hook that generates AW-owned worker protobuf stubs."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

from hatchling.builders.config import BuilderConfig
from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class CustomBuildHook(BuildHookInterface[BuilderConfig]):
    """Generate Python gRPC stubs from crates/aion-proto before packaging."""

    def initialize(self, version: str, build_data: dict[str, object]) -> None:
        generate_proto_stubs(Path(self.root))


def generate_proto_stubs(root: Path) -> None:
    proto_root = _find_repo_root(root) / "crates" / "aion-proto" / "proto"
    output = root / "aion_worker" / "proto"
    output.mkdir(parents=True, exist_ok=True)
    command = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        "-I",
        str(proto_root),
        f"--python_out={output}",
        f"--pyi_out={output}",
        f"--grpc_python_out={output}",
        str(proto_root / "common.proto"),
        str(proto_root / "worker.proto"),
    ]
    subprocess.run(command, check=True)
    _patch_relative_imports(
        output / "worker_pb2.py",
        "import common_pb2 as common__pb2",
        "from . import common_pb2 as common__pb2",
    )
    _patch_relative_imports(
        output / "worker_pb2.pyi",
        "import common_pb2 as _common_pb2",
        "from . import common_pb2 as _common_pb2",
    )
    _patch_relative_imports(
        output / "worker_pb2_grpc.py",
        "import worker_pb2 as worker__pb2",
        "from . import worker_pb2 as worker__pb2",
    )


def _find_repo_root(root: Path) -> Path:
    for candidate in (root, *root.parents):
        if (candidate / "crates" / "aion-proto" / "proto" / "worker.proto").exists():
            return candidate
    fallback = root.parents[2]
    if (fallback / "crates" / "aion-proto" / "proto" / "worker.proto").exists():
        return fallback
    raise RuntimeError("could not locate crates/aion-proto/proto/worker.proto")


def _patch_relative_imports(path: Path, old: str, new: str) -> None:
    content = path.read_text()
    path.write_text(content.replace(old, new))
