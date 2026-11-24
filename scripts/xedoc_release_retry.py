"""Bounded retries for release commands that fail with transient service errors."""

import http.client
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request


TRANSIENT_ERROR = re.compile(
    r"HTTP[^\n]*\b(?:408|429|5\d\d)\b|"
    r"(?:status|response)(?: code)?[: =]+(?:408|429|5\d\d)\b|"
    r"timed? out|timeout|connection (?:reset|refused|aborted)|"
    r"network (?:error|unreachable|connection.*lost)|"
    r"could not resolve|couldn't resolve|temporary failure|"
    r"service unavailable|bad gateway|gateway timeout|"
    r"socket hang up|ECONNRESET|ECONNREFUSED|ECONNABORTED|ETIMEDOUT|EAI_AGAIN|ENOTFOUND|"
    r"timestamp service is not available|"
    r"ticket.*not.*(?:available|found)|record not found",
    re.IGNORECASE,
)
PERMANENT_ERROR = re.compile(
    r"\b(?:HTTP[^\n]*\b(?:400|401|403|404)|"
    r"unauthorized|forbidden|invalid credentials|"
    r"no keychain password item|could not find.*profile|"
    r'status[" :]+(?:Invalid|Rejected))\b',
    re.IGNORECASE,
)


def run_with_retry(command: list[str], **kwargs) -> subprocess.CompletedProcess:
    check = kwargs.pop("check", True)
    capture_output = kwargs.pop("capture_output", False)
    stdout = kwargs.pop("stdout", None)
    kwargs.setdefault("text", True)
    for attempt in range(3):
        result = subprocess.run(
            command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs
        )
        if result.stderr and not capture_output:
            print(result.stderr, end="", file=sys.stderr, flush=True)
        retry = (
            result.returncode != 0
            and not PERMANENT_ERROR.search(result.stdout + result.stderr)
            and TRANSIENT_ERROR.search(result.stdout + result.stderr)
            and attempt < 2
        )
        if not capture_output and stdout != subprocess.DEVNULL:
            print(
                result.stdout,
                end="",
                file=sys.stderr if retry else sys.stdout,
                flush=True,
            )
        if not retry:
            if capture_output and check and result.returncode:
                print(
                    result.stdout + result.stderr, end="", file=sys.stderr, flush=True
                )
            if check:
                result.check_returncode()
            return result
        delay = 5 * (2**attempt)
        print(
            f"Transient release error; retrying in {delay}s (attempt {attempt + 2}/3).",
            file=sys.stderr,
            flush=True,
        )
        time.sleep(delay)
    raise AssertionError("unreachable")


def download_with_retry(url: str, destination: Path, *, timeout: int) -> None:
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=timeout) as response:
                with destination.open("wb") as output:
                    shutil.copyfileobj(response, output)
            return
        except urllib.error.HTTPError as error:
            error.close()
            if error.code not in {408, 429} and not 500 <= error.code < 600:
                raise
            failure = error
        except (
            urllib.error.URLError,
            http.client.HTTPException,
            ConnectionError,
            TimeoutError,
        ) as error:
            failure = error
        if attempt == 2:
            raise failure
        delay = 5 * (2**attempt)
        print(
            f"Transient download error for {url}; retrying in {delay}s "
            f"(attempt {attempt + 2}/3).",
            file=sys.stderr,
            flush=True,
        )
        time.sleep(delay)


if __name__ == "__main__":
    command = sys.argv[1:]
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        raise SystemExit("Usage: xedoc_release_retry.py -- COMMAND [ARG...]")
    try:
        raise SystemExit(run_with_retry(command, check=False).returncode)
    except OSError as error:
        raise SystemExit(str(error)) from error
