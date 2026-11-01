#!/usr/bin/env python3
"""Functional acceptance for preset copies and authenticated backend availability."""

import base64
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    router = (
        Path(sys.argv[1])
        if len(sys.argv) > 1
        else (Path(__file__).parent / "model-router" / "reference-router")
    )
    with tempfile.TemporaryDirectory(prefix="xedoc-router-copy-") as directory:
        home = Path(directory)
        environment = {**os.environ, "XEDOC_HOME": str(home)}
        catalog = json.loads(
            router.with_name("reference-router.policy.json").read_text()
        )
        work_type = catalog["axes"][0]["classes"][0]["id"]
        context = {
            "turn": {"scope": "root"},
            "availableProviders": ["openai"],
            "eligibleRoutes": [
                {
                    "providerId": "openai",
                    "model": model,
                    "reasoningEfforts": ["low", "medium", "high", "xhigh", "max"],
                }
                for model in ("gpt-6-luna", "gpt-6.1-terra", "gpt-6.1-sol")
            ],
        }

        def call(method, params=None):
            request = {
                "protocol": "xedoc.script/v1",
                "extension": "model-router",
                "requestId": "copy-acceptance",
                "method": method,
                "params": params or {},
                "context": context,
            }
            response = json.loads(
                subprocess.check_output(
                    [sys.executable, str(router)],
                    input=json.dumps(request).encode(),
                    env=environment,
                )
            )
            assert "error" not in response, response
            return response["result"]

        def act(interaction, identifier, values=None, *, kind="interaction"):
            if interaction["surface"]["type"] == "confirmation":
                assert any(
                    action["id"] == identifier and action.get("keyBindings")
                    for action in interaction["surface"]["actions"]
                ), (
                    "Selected confirmation action must be reachable through its rendered bindings"
                )
            return call(
                "interaction.respond",
                {
                    "interactionId": interaction["id"],
                    "continuation": interaction["continuation"],
                    "stateRevision": interaction["stateRevision"],
                    "outcome": "accepted",
                    "action": {"id": identifier},
                    "values": values or {},
                },
            )[kind]

        def copy_preset(name):
            surface = call("settings.open")["interaction"]
            for identifier in (
                "open-policy",
                "open-routing-tables",
                "open-table-manager",
                "open-table-openai-balanced",
                "copy-table-openai-balanced",
            ):
                surface = act(surface, identifier)
            return act(
                surface,
                "create-table",
                {
                    "table-name": name,
                    "copy-table": "openai-balanced",
                },
            )

        call("settings.open")
        path = home / "model-router" / "reference-router.policy.json"
        before = path.read_bytes()
        for providers in ([], ["openai"], ["anthropic"], ["openai", "anthropic"], []):
            context["availableProviders"] = providers
            root = call("settings.open")["interaction"]
            picker = act(root, "open-table-picker")
            tables = act(act(root, "open-policy"), "open-routing-tables")
            manager = act(tables, "open-table-manager")
            picker_ids = {item["id"] for item in picker["surface"]["items"]}
            options = next(
                field["options"]
                for field in tables["surface"]["fields"]
                if field["id"] == "shared-table"
            )
            option_ids = {option["id"] for option in options}
            manager_ids = {item["id"] for item in manager["surface"]["items"]}
            assert "default" in picker_ids & option_ids
            for provider in ("openai", "anthropic"):
                for variant in ("balanced", "smart", "cheap"):
                    name = f"{provider}-{variant}"
                    assert (name in picker_ids) == (provider in providers)
                    assert (name in option_ids) == (provider in providers)
                    assert (f"table-{name}" in manager_ids) == (provider in providers)
            assert path.read_bytes() == before
        context["availableProviders"] = ["openai"]
        policy = json.loads(path.read_text())
        balanced = next(
            curve for curve in policy["ranking"]["curves"] if curve["id"] == "balanced"
        )
        conservative = next(
            curve
            for curve in policy["ranking"]["curves"]
            if curve["id"] == "conservative"
        )
        balanced["ranges"] = copy.deepcopy(conservative["ranges"])
        original_curves = copy.deepcopy(policy["ranking"]["curves"])
        policy.update(mode="full", approval="off")
        path.write_text(json.dumps(policy))
        assert copy_preset("editable-copy")["surface"]["type"] == "form"
        copied = json.loads(path.read_text())
        assert copied["ranking"]["curves"][: len(original_curves)] == original_curves

        axes = {
            "work_type": work_type,
            "complexity": "medium",
            "risk": "medium",
            "orchestration": "none",
        }
        continuation = (
            "classifier:"
            + base64.urlsafe_b64encode(
                json.dumps(
                    {
                        "prompt": "Compare copied routing behavior",
                        "embedder": {
                            "axes": axes,
                            "evidence": {axis: [0.99, 0.9] for axis in axes},
                            "steering": False,
                            "embeddingElapsedMs": 0,
                            "embeddingInputCount": 1,
                        },
                    }
                ).encode()
            ).decode()
        )
        classifier = json.dumps(
            {
                "work_type": {"group": "group1", "steering": False},
                "complexity": "medium",
                "risk": "medium",
                "orchestration": "none",
                "orchestration_reason": "Acceptance fixture",
                "confidence": 0.99,
            }
        )
        decisions = []
        for table in ("openai-balanced", "editable-copy"):
            picker = act(call("settings.open")["interaction"], "open-table-picker")
            act(picker, f"select-table-{table}")
            result = call(
                "routing.classifier.respond",
                {
                    "continuation": continuation,
                    "output": classifier,
                },
            )["decision"]
            assert result["disposition"] == "apply", result
            decisions.append(
                {
                    "route": result["route"],
                    "rank": result["feedback"]["rankingSelectedRank"],
                }
            )
        assert decisions[0] == decisions[1], decisions

        full = json.loads(path.read_text())
        for curve in full["ranking"]["curves"]:
            curve["ranges"] = copy.deepcopy(conservative["ranges"])
        while len(full["ranking"]["curves"]) < 12:
            curve = copy.deepcopy(conservative)
            curve["id"] = f"capacity-{len(full['ranking']['curves'])}"
            full["ranking"]["curves"].append(curve)
        path.write_text(json.dumps(full))
        before = path.read_bytes()
        rejected = copy_preset("capacity-copy")
        assert rejected["surface"]["type"] == "notice", rejected
        assert "curve limit" in rejected["surface"]["body"], rejected
        assert path.read_bytes() == before
        picker = act(call("settings.open")["interaction"], "open-table-picker")
        act(picker, "select-table-openai-balanced")
        context["availableProviders"] = []
        unavailable = act(call("settings.open")["interaction"], "open-table-picker")
        assert not any(
            item["id"].startswith("openai-") for item in unavailable["surface"]["items"]
        ), unavailable
        result = call(
            "routing.classifier.respond",
            {"continuation": continuation, "output": classifier},
        )["decision"]
        assert result["route"] is None, result
        policy = json.loads(path.read_text())
        policy["approval"] = "all"
        path.write_text(json.dumps(policy))
        context["availableProviders"] = ["openai"]
        preview = call(
            "routing.classifier.respond",
            {"continuation": continuation, "output": classifier},
        )["interaction"]
        before = path.read_bytes()
        for routes in (context["eligibleRoutes"], []):
            context.update(availableProviders=[], eligibleRoutes=routes)
            rejected = act(preview, "approve", kind="decision")
            assert (
                rejected["disposition"] == "keepCurrent" and rejected["route"] is None
            )
        assert path.read_bytes() == before
        print(
            "PASS: copies retain effective curves; capacity, unavailable backends and revoked approvals fail safely"
        )


if __name__ == "__main__":
    main()
