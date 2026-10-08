"""Bounded wire contracts for broker-mediated session messages."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any, Mapping

from .errors import BrokerError
from .models import MAX_ID_LENGTH


MESSAGE_OPERATION = "session/message"
MESSAGE_CAPABILITY = "sessionMessage"
DELIVERY_DEFER = "defer"
DELIVERY_STEER = "steer"
DELIVERY_INTERRUPT = "interrupt"
DELIVERIES = {DELIVERY_DEFER, DELIVERY_STEER, DELIVERY_INTERRUPT}
_LOCAL_FIELDS = {"messageId", "correlationId", "target", "body", "delivery"}
_PEER_FIELDS = _LOCAL_FIELDS | {"source"}
_TARGET_FIELDS = {"hostId", "threadId", "activeTurnId"}
_PEER_TARGET_FIELDS = _TARGET_FIELDS - {"hostId"}
_SOURCE_FIELDS = {"hostId", "threadId", "extensionId"}
_RECEIPT_FIELDS = {
    "messageId",
    "correlationId",
    "source",
    "target",
    "delivery",
    "status",
}
_RECEIPT_SOURCE_FIELDS = {"hostId", "threadId"}
_RECEIPT_TARGET_FIELDS = {"hostId", "threadId"}
_RECEIPT_STATUSES = {"queued", "delivered", "forwarded"}


@dataclass(frozen=True)
class MessageSource:
    """Broker-derived provenance for one extension-originated message."""

    host_id: str
    thread_id: str
    extension_id: str


@dataclass(frozen=True)
class MessageTarget:
    """A selected destination and an optional exact active-turn precondition."""

    host_id: str
    thread_id: str
    active_turn_id: str | None


@dataclass(frozen=True)
class MessageSubmission:
    """Validated message data before the broker supplies provenance."""

    message_id: str | None
    correlation_id: str | None
    target: MessageTarget
    body: str
    delivery: str


def validate_local_message(
    value: Mapping[str, Any], max_body_bytes: int
) -> MessageSubmission:
    """Validate the model-visible message arguments without a source identity."""

    _exact_fields(value, _LOCAL_FIELDS, {"target", "body"})
    return _submission(value, max_body_bytes, peer=False)


def validate_peer_message(
    value: Mapping[str, Any], max_body_bytes: int
) -> tuple[MessageSource, MessageSubmission]:
    """Validate a signed peer message with explicit broker-derived provenance."""

    _exact_fields(value, _PEER_FIELDS, {"source", "target", "body"})
    source = _source(value["source"])
    submission = _submission(value, max_body_bytes, peer=True)
    return source, submission


def peer_message_params(
    source: MessageSource, submission: MessageSubmission
) -> dict[str, Any]:
    """Project a resolved local message into the fixed peer operation payload."""

    if submission.message_id is None or submission.correlation_id is None:
        raise BrokerError.internal()
    target: dict[str, str] = {"threadId": submission.target.thread_id}
    if submission.target.active_turn_id is not None:
        target["activeTurnId"] = submission.target.active_turn_id
    return {
        "messageId": submission.message_id,
        "correlationId": submission.correlation_id,
        "source": {
            "hostId": source.host_id,
            "threadId": source.thread_id,
            "extensionId": source.extension_id,
        },
        "target": target,
        "body": submission.body,
        "delivery": submission.delivery,
    }


def validate_message_receipt(
    value: Mapping[str, Any], max_result_bytes: int
) -> dict[str, Any]:
    """Validate the bounded, non-sensitive result returned to a sender."""

    _exact_fields(value, _RECEIPT_FIELDS, _RECEIPT_FIELDS)
    message_id = identifier(value["messageId"])
    correlation_id = identifier(value["correlationId"])
    source = value["source"]
    target = value["target"]
    if not isinstance(source, Mapping) or set(source) != _RECEIPT_SOURCE_FIELDS:
        raise BrokerError.invalid_request()
    if not isinstance(target, Mapping) or set(target) != _RECEIPT_TARGET_FIELDS:
        raise BrokerError.invalid_request()
    source_host_id = identifier(source["hostId"])
    source_thread_id = identifier(source["threadId"])
    target_host_id = identifier(target["hostId"])
    target_thread_id = identifier(target["threadId"])
    delivery = value["delivery"]
    status = value["status"]
    if (
        not isinstance(delivery, str)
        or delivery not in DELIVERIES
        or not isinstance(status, str)
        or status not in _RECEIPT_STATUSES
    ):
        raise BrokerError.invalid_request()
    result = {
        "messageId": message_id,
        "correlationId": correlation_id,
        "source": {"hostId": source_host_id, "threadId": source_thread_id},
        "target": {"hostId": target_host_id, "threadId": target_thread_id},
        "delivery": delivery,
        "status": status,
    }
    if _encoded_size(result) > max_result_bytes:
        raise BrokerError.limit_exceeded()
    return result


def identifier(value: Any) -> str:
    """Return one bounded opaque identifier."""

    if (
        not isinstance(value, str)
        or not value
        or len(value) > MAX_ID_LENGTH
        or any(character.isspace() or ord(character) < 33 for character in value)
    ):
        raise BrokerError.invalid_request()
    return value


def _submission(
    value: Mapping[str, Any], max_body_bytes: int, *, peer: bool
) -> MessageSubmission:
    message_id = _optional_identifier(value.get("messageId"))
    correlation_id = _optional_identifier(value.get("correlationId"))
    target = _target(value["target"], peer=peer)
    body = value["body"]
    if not isinstance(body, str) or not body:
        raise BrokerError.invalid_request()
    if len(body.encode("utf-8")) > max_body_bytes:
        raise BrokerError.limit_exceeded()
    delivery = value.get("delivery", DELIVERY_DEFER)
    if not isinstance(delivery, str) or delivery not in DELIVERIES:
        raise BrokerError.invalid_request()
    if delivery == DELIVERY_DEFER and target.active_turn_id is not None:
        raise BrokerError.invalid_request()
    if delivery != DELIVERY_DEFER and target.active_turn_id is None:
        raise BrokerError.invalid_request()
    return MessageSubmission(
        message_id=message_id,
        correlation_id=correlation_id,
        target=target,
        body=body,
        delivery=delivery,
    )


def _target(value: Any, *, peer: bool) -> MessageTarget:
    fields = _PEER_TARGET_FIELDS if peer else _TARGET_FIELDS
    required = {"threadId"} if peer else {"hostId", "threadId"}
    if not isinstance(value, Mapping):
        raise BrokerError.invalid_request()
    _exact_fields(value, fields, required)
    host_id = "" if peer else identifier(value["hostId"])
    active_turn_id = _optional_identifier(value.get("activeTurnId"))
    return MessageTarget(
        host_id=host_id,
        thread_id=identifier(value["threadId"]),
        active_turn_id=active_turn_id,
    )


def _source(value: Any) -> MessageSource:
    if not isinstance(value, Mapping) or set(value) != _SOURCE_FIELDS:
        raise BrokerError.invalid_request()
    return MessageSource(
        host_id=identifier(value["hostId"]),
        thread_id=identifier(value["threadId"]),
        extension_id=identifier(value["extensionId"]),
    )


def _optional_identifier(value: Any) -> str | None:
    if value is None:
        return None
    return identifier(value)


def _exact_fields(
    value: Mapping[str, Any], allowed: set[str], required: set[str]
) -> None:
    if set(value) - allowed or not required.issubset(value):
        raise BrokerError.invalid_request()


def _encoded_size(value: Mapping[str, Any]) -> int:
    try:
        return len(
            json.dumps(
                value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
            ).encode("utf-8")
        )
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
