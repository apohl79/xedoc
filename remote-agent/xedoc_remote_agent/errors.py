"""Stable broker errors and controller-error translation."""

from __future__ import annotations

from enum import Enum
from typing import Any


MAX_ERROR_MESSAGE_BYTES = 512


class ErrorCode(str, Enum):
    """Error codes exposed by both local IPC and the future peer protocol."""

    INVALID_REQUEST = "invalidRequest"
    UNAUTHORIZED = "unauthorized"
    NOT_FOUND = "notFound"
    CONFLICT = "conflict"
    LIMIT_EXCEEDED = "limitExceeded"
    UNAVAILABLE = "unavailable"
    INTERNAL = "internal"


_DEFAULT_MESSAGES: dict[ErrorCode, str] = {
    ErrorCode.INVALID_REQUEST: "request is invalid",
    ErrorCode.UNAUTHORIZED: "request is not authorized",
    ErrorCode.NOT_FOUND: "resource was not found",
    ErrorCode.CONFLICT: "request conflicts with current state",
    ErrorCode.LIMIT_EXCEEDED: "request exceeds a configured limit",
    ErrorCode.UNAVAILABLE: "controller is unavailable",
    ErrorCode.INTERNAL: "internal broker error",
}


class BrokerError(RuntimeError):
    """A bounded error safe to return to a local or peer caller.

    The original controller exception is intentionally not retained. Controller
    messages can contain absolute paths, prompts, or unbounded model output.
    """

    def __init__(
        self,
        code: ErrorCode,
        message: str | None = None,
        *,
        retryable: bool = False,
    ) -> None:
        self.code = ErrorCode(code)
        self.retryable = retryable
        bounded = _bound_message(message or _DEFAULT_MESSAGES[self.code])
        super().__init__(bounded)

    @property
    def message(self) -> str:
        return str(self)

    def to_dict(self) -> dict[str, Any]:
        return {
            "code": self.code.value,
            "message": self.message,
            "retryable": self.retryable,
        }

    @classmethod
    def invalid_request(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.INVALID_REQUEST, message)

    @classmethod
    def unauthorized(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.UNAUTHORIZED, message)

    @classmethod
    def not_found(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.NOT_FOUND, message)

    @classmethod
    def conflict(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.CONFLICT, message)

    @classmethod
    def limit_exceeded(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.LIMIT_EXCEEDED, message)

    @classmethod
    def unavailable(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.UNAVAILABLE, message, retryable=True)

    @classmethod
    def internal(cls, message: str | None = None) -> "BrokerError":
        return cls(ErrorCode.INTERNAL, message)


def map_controller_error(error: BaseException) -> BrokerError:
    """Translate an SDK/RPC failure into a stable, path-free broker error."""

    if isinstance(error, BrokerError):
        return error

    code = getattr(error, "code", None)
    numeric_code = code if isinstance(code, int) and not isinstance(code, bool) else None
    text = str(error).lower()

    if numeric_code in {-32600, -32602} or any(
        marker in text for marker in ("invalid request", "invalid params", "invalid parameter")
    ):
        return BrokerError.invalid_request()
    if numeric_code in {-32001, 401, 403} or any(
        marker in text for marker in ("unauthorized", "forbidden", "permission denied", "not authorized")
    ):
        return BrokerError.unauthorized()
    if numeric_code in {-32004, 404} or any(
        marker in text for marker in ("not found", "unknown thread", "no such thread")
    ):
        return BrokerError.not_found()
    if numeric_code in {-32009, 409} or any(
        marker in text for marker in ("conflict", "already active", "already exists", "busy")
    ):
        return BrokerError.conflict()
    if numeric_code in {-32008, 413, 429} or any(
        marker in text for marker in ("limit", "too large", "quota", "rate")
    ):
        return BrokerError.limit_exceeded()
    if any(
        marker in text
        for marker in (
            "closed",
            "disconnect",
            "unavailable",
            "timed out",
            "timeout",
            "connection",
            "network",
        )
    ):
        return BrokerError.unavailable()
    return BrokerError.internal()


def _bound_message(message: str) -> str:
    encoded = message.encode("utf-8", "replace")
    if len(encoded) <= MAX_ERROR_MESSAGE_BYTES:
        return message
    return encoded[: MAX_ERROR_MESSAGE_BYTES - 1].decode("utf-8", "ignore") + "…"
