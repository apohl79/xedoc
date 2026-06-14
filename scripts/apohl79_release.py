"""Build helpers for apohl79 fork release packages."""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

from codex_package.targets import TARGET_SPECS
from codex_package.targets import default_target


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
DEFAULT_REF = "main-fork"
DEFAULT_SUFFIX = "apohl79"
WORKSPACE_VERSION_SENTINEL = "0.0.0"
VERSION_RE = re.compile(
    r"^(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.(?P<patch>[0-9]+)"
    r"(?:-(?P<pre_label>alpha|beta)(?:\.(?P<pre_number>[0-9]+))?)?$"
)
RELEASE_TAG_RE = re.compile(
    r"^rust-v(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta)(?:\.[0-9]+)?)?)$"
)
LS_REMOTE_TAG_RE = re.compile(
    r"^[0-9a-fA-F]+\s+refs/tags/rust-v(?P<version>"
    r"[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta)(?:\.[0-9]+)?)?"
    r")(?:\^\{\})?$"
)
WORKSPACE_VERSION_LINE_RE = re.compile(r'^(\s*version\s*=\s*)"[^"]+"(.*)$')


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Build a signed local release package for the apohl79 Codex fork."
        ),
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--ref",
        default=DEFAULT_REF,
        help="Git ref to build. Defaults to the fork main branch.",
    )
    parser.add_argument(
        "--target",
        choices=sorted(TARGET_SPECS),
        default=default_target(),
        help="Rust target triple to package.",
    )
    parser.add_argument(
        "--base-version",
        help=(
            "Base Codex version before adding -apohl79. If omitted, the script "
            "uses codex-rs/Cargo.toml unless it is 0.0.0, then the newest "
            "reachable rust-v* tag for the build ref, then the newest valid "
            "upstream rust-v* tag."
        ),
    )
    parser.add_argument(
        "--version-suffix",
        default=DEFAULT_SUFFIX,
        help="Suffix appended to the base Codex version.",
    )
    parser.add_argument(
        "--codesign-identity",
        default=os.environ.get("APPLE_CODESIGN_IDENTITY"),
        help=(
            "Developer ID identity for native codesign. Can also be set with "
            "APPLE_CODESIGN_IDENTITY."
        ),
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("dist/apohl79"),
        help="Directory for release package output.",
    )
    parser.add_argument(
        "--package-dir",
        type=Path,
        help="Explicit package directory. Defaults under --output-dir/version.",
    )
    parser.add_argument(
        "--archive-output",
        type=Path,
        action="append",
        default=[],
        help=(
            "Archive output path. May be repeated. Defaults to a .tar.gz under "
            "--output-dir/version."
        ),
    )
    parser.add_argument(
        "--cargo",
        default="cargo",
        help="Cargo executable to use for the release build.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace existing package directory and archive outputs.",
    )
    parser.add_argument(
        "--keep-worktree",
        action="store_true",
        help="Keep the temporary patched source worktree for inspection.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        build_release(args)
    except RuntimeError as err:
        print(f"Error: {err}", file=sys.stderr)
        return 1
    return 0


def build_release(args: argparse.Namespace) -> None:
    spec = TARGET_SPECS[args.target]
    if not args.target.endswith("apple-darwin"):
        raise RuntimeError(
            "apohl79 release signing uses Apple codesign and supports only "
            "macOS targets. Pass an *-apple-darwin target."
        )

    if not args.codesign_identity:
        raise RuntimeError(
            "Must pass --codesign-identity or set APPLE_CODESIGN_IDENTITY."
        )

    output_dir = resolve_repo_path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    target_dir = Path(
        os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "codex-rs" / "target")
    )
    target_dir = target_dir.resolve()

    temp_root = Path(tempfile.mkdtemp(prefix="codex-apohl79-release-"))
    source_root = temp_root / "source"
    try:
        run(
            ["git", "worktree", "add", "--detach", str(source_root), args.ref],
            cwd=REPO_ROOT,
        )

        cargo_toml = source_root / "codex-rs" / "Cargo.toml"
        if (
            args.base_version is None
            and read_workspace_version(cargo_toml) == WORKSPACE_VERSION_SENTINEL
        ):
            run(["git", "fetch", "--quiet", "--tags", "upstream"], cwd=REPO_ROOT)

        base_version = resolve_base_version(
            cargo_toml,
            args.base_version,
            describe_tag=describe_release_tag(source_root),
            ls_remote_stdout=None,
        )
        fork_version = fork_version_from_base(base_version, args.version_suffix)
        patch_workspace_version(cargo_toml, fork_version)

        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(target_dir)
        env.setdefault("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO", "packed")
        env.setdefault("CARGO_NET_GIT_FETCH_WITH_CLI", "true")

        run(
            [
                args.cargo,
                "build",
                "--manifest-path",
                str(source_root / "codex-rs" / "Cargo.toml"),
                "--package",
                "codex-cli",
                "--bin",
                "codex",
                "--profile",
                "release",
                "--target",
                args.target,
            ],
            cwd=source_root / "codex-rs",
            env=env,
        )

        entrypoint = (
            target_dir / spec.target / "release" / f"codex{spec.exe_suffix}"
        ).resolve()
        if not entrypoint.is_file():
            raise RuntimeError(f"Built entrypoint not found: {entrypoint}")

        signing_script = (
            source_root / ".github/scripts/macos-signing/sign_macos_code.sh"
        )
        entitlements = (
            source_root / ".github/scripts/macos-signing/codex.entitlements.plist"
        )
        run(
            build_codesign_command(
                target=entrypoint,
                identity=args.codesign_identity,
                entitlements=entitlements,
                signing_script=signing_script,
            ),
            cwd=source_root,
        )
        run(["codesign", "--verify", "--strict", "--verbose=2", str(entrypoint)])

        package_dir = (
            resolve_repo_path(args.package_dir)
            if args.package_dir is not None
            else output_dir / fork_version / f"codex-package-{args.target}"
        )
        archive_outputs = [resolve_repo_path(path) for path in args.archive_output] or [
            output_dir / fork_version / f"codex-package-{args.target}.tar.gz"
        ]

        package_args = [
            sys.executable,
            str(source_root / "scripts/build_codex_package.py"),
            "--target",
            args.target,
            "--variant",
            "codex",
            "--entrypoint-bin",
            str(entrypoint),
            "--cargo-profile",
            "release",
            "--package-dir",
            str(package_dir),
        ]
        for archive_output in archive_outputs:
            package_args.extend(["--archive-output", str(archive_output)])
        if args.force:
            package_args.append("--force")

        run(package_args, cwd=source_root)

        print(f"Built apohl79 Codex release {fork_version}")
        print(f"Package directory: {package_dir}")
        for archive_output in archive_outputs:
            print(f"Archive: {archive_output}")
    finally:
        if args.keep_worktree:
            print(f"Kept temporary source worktree: {source_root}")
        else:
            if source_root.exists():
                run(
                    ["git", "worktree", "remove", "--force", str(source_root)],
                    cwd=REPO_ROOT,
                    check=False,
                )
            shutil.rmtree(temp_root, ignore_errors=True)


def resolve_base_version(
    cargo_toml: Path,
    explicit_base_version: str | None,
    *,
    describe_tag: str | None,
    ls_remote_stdout: str | None,
) -> str:
    if explicit_base_version:
        return validate_release_version(explicit_base_version)

    cargo_version = read_workspace_version(cargo_toml)
    if cargo_version != WORKSPACE_VERSION_SENTINEL:
        return validate_release_version(cargo_version)

    if describe_tag is not None:
        return base_version_from_release_tag(describe_tag)

    if ls_remote_stdout is None:
        ls_remote_stdout = subprocess.check_output(
            [
                "git",
                "ls-remote",
                "--tags",
                "--sort=v:refname",
                "upstream",
                "rust-v[0-9]*",
            ],
            cwd=REPO_ROOT,
            text=True,
        )
    return latest_release_version_from_ls_remote(ls_remote_stdout)


def derive_fork_version(
    cargo_version: str,
    *,
    describe_tag: str | None = None,
    ls_remote_stdout: str,
    suffix: str = DEFAULT_SUFFIX,
) -> str:
    if cargo_version != WORKSPACE_VERSION_SENTINEL:
        base_version = validate_release_version(cargo_version)
    elif describe_tag is not None:
        base_version = base_version_from_release_tag(describe_tag)
    else:
        base_version = latest_release_version_from_ls_remote(ls_remote_stdout)
    return fork_version_from_base(base_version, suffix)


def fork_version_from_base(base_version: str, suffix: str) -> str:
    validate_release_version(base_version)
    if not suffix:
        raise RuntimeError("Version suffix must not be empty.")
    return f"{base_version}-{suffix}"


def latest_release_version_from_ls_remote(stdout: str) -> str:
    versions = {
        match.group("version")
        for line in stdout.splitlines()
        if (match := LS_REMOTE_TAG_RE.match(line.strip())) is not None
    }
    if not versions:
        raise RuntimeError("No valid upstream rust release tags found.")
    return max(versions, key=release_version_sort_key)


def base_version_from_release_tag(tag: str) -> str:
    match = RELEASE_TAG_RE.match(tag)
    if match is None:
        raise RuntimeError(f"Invalid Codex release tag: {tag}")
    return validate_release_version(match.group("version"))


def describe_release_tag(source_root: Path) -> str | None:
    result = subprocess.run(
        [
            "git",
            "describe",
            "--tags",
            "--match",
            "rust-v[0-9]*",
            "--abbrev=0",
            "HEAD",
        ],
        cwd=source_root,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode == 0:
        return result.stdout.strip()
    return None


def release_version_sort_key(version: str) -> tuple[int, int, int, int, int]:
    match = VERSION_RE.match(version)
    if match is None:
        raise RuntimeError(f"Invalid Codex release version: {version}")

    major = int(match.group("major"))
    minor = int(match.group("minor"))
    patch = int(match.group("patch"))
    pre_label = match.group("pre_label")
    pre_number = match.group("pre_number")
    if pre_label is None:
        return (major, minor, patch, 3, 0)

    pre_rank = {"alpha": 1, "beta": 2}[pre_label]
    pre_number_value = int(pre_number) if pre_number is not None else -1
    return (major, minor, patch, pre_rank, pre_number_value)


def validate_release_version(version: str) -> str:
    if VERSION_RE.match(version) is None:
        raise RuntimeError(
            f"Invalid Codex release version: {version}. Expected x.y.z[-alpha[.N]|-beta[.N]]."
        )
    return version


def read_workspace_version(cargo_toml: Path) -> str:
    in_workspace_package = False
    with open(cargo_toml, encoding="utf-8") as fh:
        for line in fh:
            stripped = line.strip()
            if stripped == "[workspace.package]":
                in_workspace_package = True
                continue
            if in_workspace_package and stripped.startswith("["):
                break
            if in_workspace_package:
                match = WORKSPACE_VERSION_LINE_RE.match(line.rstrip("\n"))
                if match is not None:
                    return match.group(0).split('"', maxsplit=2)[1]

    raise RuntimeError(f"Could not find [workspace.package].version in {cargo_toml}")


def patch_workspace_version(cargo_toml: Path, version: str) -> None:
    validate_release_version(version.rsplit("-", maxsplit=1)[0])
    lines = cargo_toml.read_text(encoding="utf-8").splitlines(keepends=True)
    in_workspace_package = False
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped == "[workspace.package]":
            in_workspace_package = True
            continue
        if in_workspace_package and stripped.startswith("["):
            break
        if in_workspace_package:
            line_without_newline = line.rstrip("\r\n")
            newline = line[len(line_without_newline) :]
            match = WORKSPACE_VERSION_LINE_RE.match(line_without_newline)
            if match is not None:
                lines[index] = f'{match.group(1)}"{version}"{match.group(2)}{newline}'
                cargo_toml.write_text("".join(lines), encoding="utf-8")
                return

    raise RuntimeError(f"Could not find [workspace.package].version in {cargo_toml}")


def build_codesign_command(
    *,
    target: Path,
    identity: str,
    entitlements: Path,
    signing_script: Path = Path(".github/scripts/macos-signing/sign_macos_code.sh"),
) -> list[str]:
    return [
        str(signing_script),
        "--target",
        str(target),
        "--identity",
        identity,
        "--deep",
        "false",
        "--identifier",
        "codex",
        "--options",
        "runtime",
        "--timestamp",
        "true",
        "--entitlements",
        str(entitlements),
    ]


def resolve_repo_path(path: Path) -> Path:
    if path.is_absolute():
        return path
    return (REPO_ROOT / path).resolve()


def run(
    command: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess:
    print("+ " + " ".join(command), flush=True)
    return subprocess.run(command, cwd=cwd, env=env, check=check)
