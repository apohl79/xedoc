#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
router="$script_dir/model-router/reference-router"
request_file="$(mktemp "${TMPDIR:-/tmp}/xedoc-model-router-report.XXXXXX")"
readonly request_file

cleanup() {
  rm -f "$request_file"
}

trap cleanup EXIT

python3 - "$request_file" <<'PY'
import json
import sys

day = 1_790_380_800
json.dump(
    {
        "protocol": "xedoc.script/v1",
        "extension": "model-router",
        "requestId": "report-coverage-regression",
        "method": "report.render",
        "context": {
            "client": {"kind": "browser", "surfaces": ["reportDocument"]},
            "renderer": {"version": 1},
        },
        "params": {
            "report": {
                "fromDay": day,
                "throughDay": day,
                "daysTruncated": False,
                "recentDecisions": [],
                "days": [
                    {
                        "day": day,
                        "providerId": "openai",
                        "modelSlug": "actual-model",
                        "scope": "root",
                        "reasoningEffort": "medium",
                        "invocations": 1,
                        "totalCostUsd": 3.0,
                        "normalizedBaselineUsd": 5.0,
                        "estimatedSavingsUsd": 2.0,
                        "abExperimentOverheadUsd": None,
                        "inputTokens": 1,
                        "cachedInputTokens": 0,
                        "outputTokens": 1,
                        "missingUsageInvocations": 0,
                        "unknownPriceInvocations": 0,
                        "unknownBaselinePriceInvocations": 0,
                    },
                    {
                        "day": day,
                        "providerId": "openai",
                        "modelSlug": "older-model",
                        "scope": "unattributed",
                        "reasoningEffort": "medium",
                        "invocations": 1,
                        "totalCostUsd": None,
                        "normalizedBaselineUsd": None,
                        "estimatedSavingsUsd": None,
                        "abExperimentOverheadUsd": None,
                        "inputTokens": 1,
                        "cachedInputTokens": 0,
                        "outputTokens": 1,
                        "missingUsageInvocations": 0,
                        "unknownPriceInvocations": 1,
                        "unknownBaselinePriceInvocations": 1,
                    },
                ],
            }
        },
    },
    open(sys.argv[1], "w", encoding="utf-8"),
)
PY

python3 "$router" <"$request_file" | python3 -c '
import json
import sys

document = json.load(sys.stdin)["result"]["report"]
metrics = {
    metric["label"]: metric["value"]
    for section in document["sections"]
    if section["kind"] == "metricGrid"
    for metric in section["metrics"]
}
assert metrics["Actual cost"] == "$3.00", metrics
assert metrics["Baseline cost"] == "$5.00", metrics
assert metrics["Estimated savings"] == "$2.00", metrics
assert metrics["Estimated savings rate (priced usage)"] == "40.0%", metrics
chart = next(
    section for section in document["sections"] if section["kind"] == "lineChart"
)
series = {line["label"]: line["points"] for line in chart["series"]}
assert series["Actual cost"][0]["value"] == 3.0, series
assert series["Estimated baseline (priced usage)"][0]["value"] == 5.0, series
assert series["Estimated savings (priced usage)"][0]["value"] == 2.0, series
daily_activity = next(
    section
    for section in document["sections"]
    if section["kind"] == "table" and section["title"] == "Daily activity"
)
assert "Savings rate (priced usage)" in daily_activity["columns"], daily_activity
'
