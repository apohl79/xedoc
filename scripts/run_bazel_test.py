#!/usr/bin/env python3
"""Run a focused Bazel target and reject empty Rust test selections."""

import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from urllib.parse import urlparse
from urllib.request import url2pathname


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="xedoc-test-") as directory:
        events = Path(directory) / "events.json"
        result = subprocess.run(
            [
                "bazel",
                "test",
                "--test_output=errors",
                f"--build_event_json_file={events}",
                *sys.argv[1:],
            ],
            check=False,
        )
        if result.returncode:
            return result.returncode

        counts = []
        for line in events.read_text().splitlines():
            event = json.loads(line)
            for output in event.get("testResult", {}).get("testActionOutput", []):
                if output.get("name") != "test.log":
                    continue
                uri = urlparse(output.get("uri", ""))
                if uri.scheme != "file":
                    continue
                log = Path(url2pathname(uri.path)).read_text(errors="replace")
                counts.extend(
                    (int(passed), int(failed))
                    for passed, failed in re.findall(
                        r"test result: \w+\. (\d+) passed; (\d+) failed;", log
                    )
                )
        if counts:
            executed = sum(passed + failed for passed, failed in counts)
            if executed == 0:
                print(
                    "ERROR: Rust test selection executed zero cases.", file=sys.stderr
                )
                return 1
            print(f"Verified Rust test execution: {executed} cases.")
        elif "--test_arg=--exact" in sys.argv[1:]:
            print("ERROR: Cannot verify exact Rust test execution.", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
