#!/usr/bin/env python3
"""Build a Developer ID-signed local Xedoc binary outside Bazel's output tree."""

import argparse
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

SCRIPT_DIR = Path(__file__).resolve().parent

if sys.version_info < (3, 10):
    uv = shutil.which("uv")
    if uv is None:
        print("Error: Python 3.10+ or uv is required.", file=sys.stderr)
        raise SystemExit(1)
    os.execvp(
        uv,
        [
            uv,
            "run",
            "--frozen",
            "--project",
            str(SCRIPT_DIR),
            "python",
            str(Path(__file__).resolve()),
            *sys.argv[1:],
        ],
    )

from xedoc_release import build_codesign_command
from xedoc_release import resolve_codesign_identity


REPO_ROOT = SCRIPT_DIR.parent
DEFAULT_OUTPUT = Path("dist/xedoc/dev/xedoc")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a Developer ID-signed local Xedoc binary for macOS.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--bazel", default="bazel", help="Bazel executable to use.")
    parser.add_argument(
        "--codesign-identity",
        default=os.environ.get("APPLE_CODESIGN_IDENTITY"),
        help=(
            "Developer ID identity. Defaults to APPLE_CODESIGN_IDENTITY or the "
            "sole Developer ID Application identity in the keychain."
        ),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help="Staged signed binary path, relative to the repository by default.",
    )
    return parser.parse_args(argv)


def resolve_repo_path(path: Path) -> Path:
    return path if path.is_absolute() else (REPO_ROOT / path).resolve()


def run(command: list[str]) -> None:
    subprocess.run(command, check=True, cwd=REPO_ROOT)


def bazel_binary_path(bazel: str) -> Path:
    bazel_bin = Path(
        subprocess.check_output(
            [bazel, "info", "bazel-bin"], cwd=REPO_ROOT, text=True
        ).strip()
    )
    binary = bazel_bin / "xedoc-rs" / "cli" / "xedoc"
    if not binary.is_file():
        raise RuntimeError(f"Bazel did not produce the expected Xedoc binary: {binary}")
    return binary


def build_signed_local_binary(args: argparse.Namespace) -> Path:
    if platform.system() != "Darwin":
        raise RuntimeError("Signed local Xedoc builds are supported only on macOS.")

    run([args.bazel, "build", "//xedoc-rs/cli:xedoc"])
    source = bazel_binary_path(args.bazel)
    output = resolve_repo_path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    identity = resolve_codesign_identity(args.codesign_identity)
    entitlements = REPO_ROOT / ".github/scripts/macos-signing/xedoc.entitlements.plist"
    signing_script = REPO_ROOT / ".github/scripts/macos-signing/sign_macos_code.sh"

    with tempfile.NamedTemporaryFile(
        dir=output.parent, prefix=".xedoc.", delete=False
    ) as file:
        staged = Path(file.name)
    try:
        shutil.copy2(source, staged)
        run(
            build_codesign_command(
                target=staged,
                identity=identity,
                entitlements=entitlements,
                signing_script=signing_script,
            )
        )
        run(["codesign", "--verify", "--strict", "--verbose=2", str(staged)])
        staged.replace(output)
    finally:
        staged.unlink(missing_ok=True)

    return output


def main(argv: list[str] | None = None) -> int:
    try:
        output = build_signed_local_binary(parse_args(argv))
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
