#!/usr/bin/env python3

from importlib.machinery import SourceFileLoader
import importlib.util
import sys
from pathlib import Path
import unittest

SCRIPT_PATH = Path(__file__).with_name("codex-session")
SCRIPT_SPEC = importlib.util.spec_from_loader(
    "session_control",
    SourceFileLoader("session_control", str(SCRIPT_PATH)),
)
if SCRIPT_SPEC is None or SCRIPT_SPEC.loader is None:
    raise RuntimeError(f"Could not load {SCRIPT_PATH}")
session_control = importlib.util.module_from_spec(SCRIPT_SPEC)
sys.modules["session_control"] = session_control
SCRIPT_SPEC.loader.exec_module(session_control)

build_turn_start_params = session_control.build_turn_start_params
build_turn_steer_params = session_control.build_turn_steer_params
list_running_turns = session_control.list_running_turns
list_sessions = session_control.list_sessions
send_turn = session_control.send_turn
start_turn = session_control.start_turn
steer_turn = session_control.steer_turn


class FakeClient:
    def __init__(self, responses: list[dict[str, object]]) -> None:
        self.responses = iter(responses)
        self.requests: list[tuple[str, dict[str, object]]] = []

    def request(self, method: str, params: dict[str, object]) -> dict[str, object]:
        self.requests.append((method, params))
        return next(self.responses)


class SessionControlTest(unittest.TestCase):
    def test_start_params_include_text_and_optional_client_id(self) -> None:
        self.assertEqual(
            build_turn_start_params("thr-1", "Run tests", "msg-1"),
            {
                "threadId": "thr-1",
                "input": [{"type": "text", "text": "Run tests"}],
                "clientUserMessageId": "msg-1",
            },
        )

    def test_steer_params_include_expected_turn_id(self) -> None:
        self.assertEqual(
            build_turn_steer_params("thr-1", "turn-1", "Focus on tests", None),
            {
                "threadId": "thr-1",
                "input": [{"type": "text", "text": "Focus on tests"}],
                "expectedTurnId": "turn-1",
            },
        )

    def test_list_sessions_reads_each_loaded_thread(self) -> None:
        client = FakeClient(
            [
                {"data": ["thr-1"], "nextCursor": None},
                {
                    "thread": {
                        "id": "thr-1",
                        "name": "Build session",
                        "cwd": "/workspace/project",
                        "status": {"type": "active", "activeFlags": []},
                        "canAcceptDirectInput": True,
                    }
                },
            ]
        )

        self.assertEqual(
            list_sessions(client),
            [
                {
                    "id": "thr-1",
                    "name": "Build session",
                    "cwd": "/workspace/project",
                    "status": {"type": "active", "activeFlags": []},
                    "canAcceptDirectInput": True,
                }
            ],
        )
        self.assertEqual(
            client.requests,
            [
                ("thread/loaded/list", {}),
                ("thread/read", {"threadId": "thr-1", "includeTurns": False}),
            ],
        )

    def test_list_running_turns_filters_completed_turns(self) -> None:
        client = FakeClient(
            [
                {
                    "thread": {
                        "turns": [
                            {"id": "turn-done", "status": "completed"},
                            {
                                "id": "turn-live",
                                "status": "inProgress",
                                "startedAt": 123,
                            },
                        ]
                    }
                }
            ]
        )

        self.assertEqual(
            list_running_turns(client, "thr-1"),
            [{"id": "turn-live", "status": "inProgress", "startedAt": 123}],
        )
        self.assertEqual(
            client.requests,
            [("thread/read", {"threadId": "thr-1", "includeTurns": True})],
        )

    def test_start_turn_resumes_thread_before_sending_prompt(self) -> None:
        client = FakeClient(
            [
                {"thread": {"turns": []}},
                {"turn": {"id": "turn-1", "status": "inProgress"}},
            ]
        )

        result = start_turn(client, "thr-1", "Run tests", "msg-1")

        self.assertEqual(result, {"turn": {"id": "turn-1", "status": "inProgress"}})
        self.assertEqual(
            client.requests,
            [
                ("thread/resume", {"threadId": "thr-1"}),
                (
                    "turn/start",
                    {
                        "threadId": "thr-1",
                        "input": [{"type": "text", "text": "Run tests"}],
                        "clientUserMessageId": "msg-1",
                    },
                ),
            ],
        )

    def test_steer_turn_resumes_thread_before_sending_expected_turn(self) -> None:
        client = FakeClient([{"thread": {"turns": []}}, {"turnId": "turn-1"}])

        result = steer_turn(client, "thr-1", "turn-1", "Focus on tests", None)

        self.assertEqual(result, {"turnId": "turn-1"})
        self.assertEqual(
            client.requests,
            [
                ("thread/resume", {"threadId": "thr-1"}),
                (
                    "turn/steer",
                    {
                        "threadId": "thr-1",
                        "input": [{"type": "text", "text": "Focus on tests"}],
                        "expectedTurnId": "turn-1",
                    },
                ),
            ],
        )

    def test_send_turn_steers_resumed_thread_with_active_turn(self) -> None:
        client = FakeClient(
            [
                {"thread": {"turns": [{"id": "turn-live", "status": "inProgress"}]}},
                {"turnId": "turn-live"},
            ]
        )

        result = send_turn(client, "thr-1", "Focus on tests", "msg-1")

        self.assertEqual(result, {"turnId": "turn-live"})
        self.assertEqual(
            client.requests,
            [
                ("thread/resume", {"threadId": "thr-1"}),
                (
                    "turn/steer",
                    {
                        "threadId": "thr-1",
                        "input": [{"type": "text", "text": "Focus on tests"}],
                        "expectedTurnId": "turn-live",
                        "clientUserMessageId": "msg-1",
                    },
                ),
            ],
        )

    def test_send_turn_starts_resumed_thread_without_active_turn(self) -> None:
        client = FakeClient(
            [
                {"thread": {"turns": [{"id": "turn-done", "status": "completed"}]}},
                {"turn": {"id": "turn-new", "status": "inProgress"}},
            ]
        )

        result = send_turn(client, "thr-1", "Run tests", None)

        self.assertEqual(result, {"turn": {"id": "turn-new", "status": "inProgress"}})
        self.assertEqual(
            client.requests,
            [
                ("thread/resume", {"threadId": "thr-1"}),
                (
                    "turn/start",
                    {
                        "threadId": "thr-1",
                        "input": [{"type": "text", "text": "Run tests"}],
                    },
                ),
            ],
        )


if __name__ == "__main__":
    unittest.main()
