"""Activity failure classes with explicit retry classification."""

from __future__ import annotations

from typing import Any

from .session import Payload


class ActivityFailureError(Exception):
    """Base class for SDK-classified activity failures."""

    def __init__(self, message: str, *, detail: Payload | Any | None = None) -> None:
        super().__init__(message)
        self.detail = detail


class RetryableError(ActivityFailureError):
    """Raise from a handler when the activity failure should be retried."""


class TerminalError(ActivityFailureError):
    """Raise from a handler when the activity failure should not be retried."""
