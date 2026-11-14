"""Generate or publish the stable Xedoc app cask from a real release archive."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess


def generate_cask(*, archive: Path, version: str, repo: str) -> str:
    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is None:
        raise RuntimeError("Homebrew casks require a stable x.y.z release.")
    if re.fullmatch(r"[\w.-]+/[\w.-]+", repo) is None:
        raise RuntimeError("Invalid GitHub release repository.")
    digest = hashlib.sha256()
    with archive.open("rb") as archive_file:
        for chunk in iter(lambda: archive_file.read(1024 * 1024), b""):
            digest.update(chunk)
    return f'''cask "xedoc" do
  version "{version}"
  sha256 "{digest.hexdigest()}"

  url "https://github.com/{repo}/releases/download/v#{{version}}/xedoc-macos-#{{version}}.zip"
  name "Xedoc"
  desc "Native client for Xedoc hosts"
  homepage "https://github.com/{repo}"

  depends_on macos: :sonoma

  app "Xedoc.app"
end
'''


def publish_cask(
    *, content: str, version: str, gh: str, tap: str, env: dict[str, str] | None
) -> None:
    endpoint = f"repos/{tap}/contents/Casks/xedoc.rb"
    try:
        existing = subprocess.run(
            [gh, "api", endpoint], text=True, capture_output=True, env=env
        )
    except OSError as err:
        raise RuntimeError("Could not read the Homebrew cask.") from err
    payload = {
        "message": f"chore: update Xedoc to {version}",
        "content": base64.b64encode(content.encode()).decode(),
    }
    if existing.returncode == 0:
        current = json.loads(existing.stdout)
        if base64.b64decode(current["content"]).decode() == content:
            return
        payload["sha"] = current["sha"]
    elif "HTTP 404" not in existing.stderr:
        raise RuntimeError(f"Could not read Homebrew cask: {existing.stderr.strip()}")
    try:
        subprocess.run(
            [gh, "api", "--method", "PUT", endpoint, "--input", "-"],
            input=json.dumps(payload),
            text=True,
            check=True,
            env=env,
            stdout=subprocess.DEVNULL,
        )
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError("Could not publish the Homebrew cask.") from err


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repo", default="apohl79/xedoc")
    parser.add_argument("--tap", default="apohl79/homebrew-xedoc")
    parser.add_argument("--gh", default="gh")
    parser.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    content = generate_cask(archive=args.archive, version=args.version, repo=args.repo)
    if args.publish:
        publish_cask(
            content=content, version=args.version, gh=args.gh, tap=args.tap, env=None
        )
    else:
        print(content, end="")


if __name__ == "__main__":
    main()
