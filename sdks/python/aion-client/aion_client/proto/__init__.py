"""Generated AW workflow API stubs (workflow.proto + common.proto).

Regenerate from the repository proto source with::

    python -m grpc_tools.protoc -I crates/aion-proto/proto \
        --python_out=sdks/python/aion-client/aion_client/proto \
        --grpc_python_out=sdks/python/aion-client/aion_client/proto \
        --pyi_out=sdks/python/aion-client/aion_client/proto \
        workflow.proto common.proto

then rewrite the generated top-level imports to package-relative
(``import common_pb2`` -> ``from . import common_pb2``), matching the
aion-worker SDK's committed-stub convention.
"""

from . import common_pb2, common_pb2_grpc, workflow_pb2, workflow_pb2_grpc

__all__ = ["common_pb2", "common_pb2_grpc", "workflow_pb2", "workflow_pb2_grpc"]
