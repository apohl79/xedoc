"""Stable system hostname for host GUI identity."""

import socket
import subprocess
import sys


def host_hostname() -> str:
    if sys.platform == "darwin":
        try:
            result = subprocess.run(
                ["/usr/sbin/scutil", "--get", "LocalHostName"],
                check=True,
                capture_output=True,
                text=True,
                timeout=2,
            )
            hostname = result.stdout.strip()
            if hostname:
                return hostname
        except (OSError, subprocess.SubprocessError, UnicodeError):
            pass
    return socket.gethostname().split(".", 1)[0]
