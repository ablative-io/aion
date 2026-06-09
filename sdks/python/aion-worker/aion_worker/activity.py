"""@activity decorator, type-hint codec, registry."""

from __future__ import annotations

import asyncio
import inspect
import json
import logging
from collections.abc import Callable, Iterable, Mapping
from concurrent.futures import Executor
from dataclasses import dataclass
from typing import get_type_hints

from .context import JSON_CONTENT_TYPE, ActivityContext, encode_detail
from .errors import ActivityFailureError, RetryableError, TerminalError
from .loop import Completed, DispatchOutcome, Failed
from .proto import common_pb2, worker_pb2
from .session import ActivityError, ActivityTask, Payload

logger = logging.getLogger(__name__)


class ActivityRegistrationError(ValueError):
    """Raised when activity registration is invalid."""


@dataclass(frozen=True)
class _ActivitySpec:
    name: str
    handler: Callable[..., object]
    input_parameter: str | None
    context_parameter: str | None
    is_async: bool


class ActivityRegistry:
    """Registry of typed activity handlers keyed by activity type name."""

    def __init__(self, *, executor: Executor | None = None) -> None:
        self._activities: dict[str, _ActivitySpec] = {}
        self._executor = executor

    def activity(self, *, name: str) -> Callable[[Callable[..., object]], Callable[..., object]]:
        """Register a function as an activity under the supplied wire name."""

        def decorate(func: Callable[..., object]) -> Callable[..., object]:
            self.register(name=name, func=func)
            return func

        return decorate

    def register(self, *, name: str, func: Callable[..., object]) -> None:
        """Register one activity function."""

        if name in self._activities:
            raise ActivityRegistrationError(f"activity {name!r} is already registered")
        self._activities[name] = _build_spec(name, func)

    def activity_types(self) -> Iterable[str]:
        """Return deterministic activity type names."""

        return sorted(self._activities)

    async def dispatch(self, task: ActivityTask, context: ActivityContext) -> DispatchOutcome:
        """Decode input, invoke the registered handler, and encode the result."""

        spec = self._activities.get(task.activity_type)
        if spec is None:
            message = f"missing activity handler {task.activity_type!r}"
            return Failed(_activity_error(worker_pb2.ACTIVITY_ERROR_KIND_TERMINAL, message))
        try:
            argument = decode_json_payload(task.input)
        except (UnicodeDecodeError, json.JSONDecodeError, TypeError) as exc:
            return Failed(
                _activity_error(worker_pb2.ACTIVITY_ERROR_KIND_TERMINAL, f"failed to decode activity input: {exc}")
            )
        try:
            result = await self._invoke(spec, argument, context)
            return Completed(encode_json_payload(result, task.input.content_type))
        except RetryableError as exc:
            return Failed(_classified_error(worker_pb2.ACTIVITY_ERROR_KIND_RETRYABLE, exc, task.input.content_type))
        except TerminalError as exc:
            return Failed(_classified_error(worker_pb2.ACTIVITY_ERROR_KIND_TERMINAL, exc, task.input.content_type))
        except Exception as exc:
            logger.warning(
                "unclassified activity exception escaped handler; reporting as retryable",
                exc_info=True,
                extra={"activity_type": task.activity_type},
            )
            return Failed(_activity_error(worker_pb2.ACTIVITY_ERROR_KIND_RETRYABLE, str(exc)))

    async def _invoke(self, spec: _ActivitySpec, argument: object, context: ActivityContext) -> object:
        args = _handler_arguments(spec, argument, context)
        if spec.is_async:
            result = spec.handler(*args)
            return await _await_handler_result(result)
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self._executor, _call_sync, spec.handler, args)


def activity(*, name: str) -> Callable[[Callable[..., object]], Callable[..., object]]:
    """Register a function in the process-global activity registry."""

    return default_registry.activity(name=name)


def encode_json_payload(value: object, content_type: str = JSON_CONTENT_TYPE) -> Payload:
    """Encode a Python value as a JSON Payload preserving the requested content type."""

    return common_pb2.Payload(content_type=content_type, bytes=json.dumps(value).encode("utf-8"))


def decode_json_payload(payload: Payload) -> object:
    """Decode a JSON Payload using its bytes without mutating its content type."""

    if payload.content_type != JSON_CONTENT_TYPE:
        raise TypeError(f"unsupported payload content type {payload.content_type!r}")
    text = payload.bytes.decode("utf-8")
    return json.loads(text)


def _build_spec(name: str, func: Callable[..., object]) -> _ActivitySpec:
    signature = inspect.signature(func)
    parameters = list(signature.parameters.values())
    variadic_kinds = {inspect.Parameter.VAR_POSITIONAL, inspect.Parameter.VAR_KEYWORD}
    if any(parameter.kind in variadic_kinds for parameter in parameters):
        raise ActivityRegistrationError(f"activity {name!r} must not use *args or **kwargs")
    hints = get_type_hints(func)
    input_parameter: str | None = None
    context_parameter: str | None = None
    for parameter in parameters:
        annotation = hints.get(parameter.name, parameter.annotation)
        if annotation is ActivityContext or parameter.name in {"ctx", "context"}:
            if context_parameter is not None:
                raise ActivityRegistrationError(f"activity {name!r} must accept at most one context parameter")
            context_parameter = parameter.name
        elif input_parameter is None:
            input_parameter = parameter.name
        else:
            raise ActivityRegistrationError(f"activity {name!r} must accept at most one input parameter plus context")
    if input_parameter is not None and input_parameter not in hints:
        raise ActivityRegistrationError(f"activity {name!r} input parameter {input_parameter!r} must have a type hint")
    if signature.return_annotation is inspect.Signature.empty and "return" not in hints:
        raise ActivityRegistrationError(f"activity {name!r} must have a return type hint")
    return _ActivitySpec(
        name=name,
        handler=func,
        input_parameter=input_parameter,
        context_parameter=context_parameter,
        is_async=inspect.iscoroutinefunction(func),
    )


def _handler_arguments(spec: _ActivitySpec, argument: object, context: ActivityContext) -> tuple[object, ...]:
    values: list[object] = []
    signature = inspect.signature(spec.handler)
    for parameter in signature.parameters.values():
        if parameter.name == spec.context_parameter:
            values.append(context)
        elif parameter.name == spec.input_parameter:
            values.append(argument)
    return tuple(values)


async def _await_handler_result(result: object) -> object:
    if inspect.isawaitable(result):
        return await result
    return result


def _call_sync(handler: Callable[..., object], args: tuple[object, ...]) -> object:
    return handler(*args)


def _classified_error(
    kind: worker_pb2.ActivityErrorKind,
    exc: ActivityFailureError,
    content_type: str,
) -> ActivityError:
    return _activity_error(kind, str(exc), encode_detail(exc.detail, content_type))


def _activity_error(kind: worker_pb2.ActivityErrorKind, message: str, details: Payload | None = None) -> ActivityError:
    error = worker_pb2.ActivityError(kind=kind, message=message)
    if details is not None:
        error.details.CopyFrom(details)
    return error


def clone_registry(registry: ActivityRegistry, *, executor: Executor | None = None) -> ActivityRegistry:
    """Clone activity registrations so a Worker can attach its executor."""

    cloned = ActivityRegistry(executor=executor if executor is not None else registry._executor)
    for name, spec in registry._activities.items():
        cloned._activities[name] = spec
    return cloned


def registry_from_mapping(
    activities: Mapping[str, Callable[..., object]],
    *,
    executor: Executor | None = None,
) -> ActivityRegistry:
    """Build a registry from an explicit mapping of activity names to handlers."""

    registry = ActivityRegistry(executor=executor)
    for name, func in activities.items():
        registry.register(name=name, func=func)
    return registry


default_registry = ActivityRegistry()
