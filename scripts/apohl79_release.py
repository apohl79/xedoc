"""Build helpers for apohl79 fork release packages."""

import argparse
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys

from codex_package.targets import TARGET_SPECS
from codex_package.targets import default_target


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
DEFAULT_REF = "main-fork"
DEFAULT_SUFFIX = "apohl79"
DEFAULT_GITHUB_REPO = "apohl79/codex"
PLACEHOLDER_CODESIGN_IDENTITY = "Developer ID Application: YOUR NAME (TEAMID)"
DEVELOPER_ID_APPLICATION_PREFIX = "Developer ID Application:"
WORKSPACE_VERSION_SENTINEL = "0.0.0"
VERSION_RE = re.compile(
    r"^(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.(?P<patch>[0-9]+)"
    r"(?:-(?P<pre_label>alpha|beta)(?:\.(?P<pre_number>[0-9]+))?)?$"
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
        "--version-suffix",
        default=DEFAULT_SUFFIX,
        help="Suffix appended to the base Codex version.",
    )
    parser.add_argument(
        "--codesign-identity",
        default=os.environ.get("APPLE_CODESIGN_IDENTITY"),
        help=(
            "Developer ID identity for native codesign. Can also be set with "
            "APPLE_CODESIGN_IDENTITY. Defaults to the sole Developer ID "
            "Application identity in the keychain."
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
            "Archive output path. May be repeated. Defaults to a versioned "
            ".zip under --output-dir/version."
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
        "--github-repo",
        default=DEFAULT_GITHUB_REPO,
        help="GitHub repository that receives the release and uploaded archives.",
    )
    parser.add_argument(
        "--gh",
        default="gh",
        help="GitHub CLI executable used for release publishing.",
    )
    parser.add_argument(
        "--skip-github-release",
        action="store_true",
        help="Build the package without creating or uploading a GitHub release.",
    )
    parser.add_argument(
        "--keep-worktree",
        action="store_true",
        help=(
            "Deprecated compatibility flag. Release builds now use the current "
            "checkout directly so Cargo can reuse incremental artifacts."
        ),
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

    codesign_identity = resolve_codesign_identity(args.codesign_identity)

    output_dir = resolve_repo_path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    target_dir = Path(
        os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "codex-rs" / "target")
    )
    target_dir = target_dir.resolve()

    source_root = REPO_ROOT
    cargo_toml = source_root / "codex-rs" / "Cargo.toml"
    cargo_lock = source_root / "codex-rs" / "Cargo.lock"
    ensure_current_checkout_matches_ref(args.ref)
    ensure_git_path_clean(cargo_toml)
    ensure_git_path_clean(cargo_lock)
    base_version = resolve_base_version(cargo_toml, ls_remote_stdout=None)
    repair_stale_release_lockfiles(
        cargo=args.cargo,
        source_root=source_root,
        cargo_toml=cargo_toml,
        cargo_lock=cargo_lock,
        target=args.target,
    )
    fork_version = fork_version_from_base(base_version, args.version_suffix)

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["CODEX_RELEASE_VERSION"] = fork_version
    env.setdefault("CARGO_BUILD_JOBS", str(default_cargo_build_jobs()))
    env.setdefault("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO", "packed")
    env.setdefault("CARGO_NET_GIT_FETCH_WITH_CLI", "true")

    run(
        [
            args.cargo,
            "build",
            "--locked",
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

    signing_script = source_root / ".github/scripts/macos-signing/sign_macos_code.sh"
    entitlements = (
        source_root / ".github/scripts/macos-signing/codex.entitlements.plist"
    )
    run(
        build_codesign_command(
            target=entrypoint,
            identity=codesign_identity,
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
        output_dir / fork_version / f"codex-{args.target}-{fork_version}.zip"
    ]

    package_args = [
        sys.executable,
        str(source_root / "scripts/build_codex_package.py"),
        "--target",
        args.target,
        "--variant",
        "codex",
        "--version",
        fork_version,
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

    release_tag = github_release_tag(fork_version)
    if not args.skip_github_release:
        publish_github_release(
            gh=args.gh,
            repo=args.github_repo,
            tag=release_tag,
            title=fork_version,
            target=git_commit("HEAD"),
            archive_outputs=archive_outputs,
        )

    print(f"Built apohl79 Codex release {fork_version}")
    print(f"GitHub release: {release_tag}")
    print(f"Package directory: {package_dir}")
    for archive_output in archive_outputs:
        print(f"Archive: {archive_output}")


def ensure_current_checkout_matches_ref(ref: str) -> None:
    head_commit = git_commit("HEAD")
    ref_commit = git_commit(ref)
    if head_commit != ref_commit:
        raise RuntimeError(
            f"Current checkout HEAD ({head_commit[:12]}) does not match "
            f"--ref {ref} ({ref_commit[:12]}). Check out {ref} before running "
            "the incremental apohl79 release build."
        )


def git_commit(ref: str) -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--verify", f"{ref}^{{commit}}"],
            cwd=REPO_ROOT,
            text=True,
        ).strip()
    except subprocess.CalledProcessError as err:
        raise RuntimeError(f"Could not resolve git ref {ref!r} to a commit.") from err


def ensure_git_path_clean(path: Path) -> None:
    relative_path = path.relative_to(REPO_ROOT)
    for diff_args in (["diff", "--quiet"], ["diff", "--cached", "--quiet"]):
        result = subprocess.run(
            ["git", *diff_args, "--", str(relative_path)],
            cwd=REPO_ROOT,
            check=False,
        )
        if result.returncode == 1:
            raise RuntimeError(
                f"{relative_path} has local changes. Commit or stash them before "
                "running the apohl79 release build."
            )
        if result.returncode != 0:
            raise RuntimeError(f"Could not check git status for {relative_path}.")


def repair_stale_release_lockfiles(
    *,
    cargo: str,
    source_root: Path,
    cargo_toml: Path,
    cargo_lock: Path,
    target: str,
) -> None:
    expected_version = read_workspace_version(cargo_toml)
    if expected_version == WORKSPACE_VERSION_SENTINEL:
        return

    stale_packages = stale_workspace_lock_packages(cargo_lock, expected_version)
    if not stale_packages:
        return

    preview = ", ".join(stale_packages[:5])
    if len(stale_packages) > 5:
        preview = f"{preview}, ..."
    print(
        "Cargo.lock workspace package versions are stale; regenerating "
        f"for {expected_version} ({preview}).",
        flush=True,
    )

    run(
        [
            cargo,
            "metadata",
            "--manifest-path",
            str(cargo_toml),
            "--format-version=1",
            "--filter-platform",
            target,
        ],
        cwd=source_root / "codex-rs",
        stdout=subprocess.DEVNULL,
    )
    refresh_bazel_lockfiles(source_root)

    remaining_stale_packages = stale_workspace_lock_packages(
        cargo_lock, expected_version
    )
    if remaining_stale_packages:
        preview = ", ".join(remaining_stale_packages[:5])
        if len(remaining_stale_packages) > 5:
            preview = f"{preview}, ..."
        raise RuntimeError(
            "Cargo.lock still has stale workspace package versions after "
            f"regeneration: {preview}"
        )


def stale_workspace_lock_packages(cargo_lock: Path, expected_version: str) -> list[str]:
    stale_packages: list[str] = []
    for name, version in read_path_lock_packages(cargo_lock):
        if version != expected_version:
            stale_packages.append(f"{name}={version}")
    return stale_packages


def read_path_lock_packages(cargo_lock: Path) -> list[tuple[str, str]]:
    packages: list[tuple[str, str]] = []
    current_name: str | None = None
    current_version: str | None = None
    current_has_source = False

    def finish_package() -> None:
        if (
            current_name is not None
            and current_version is not None
            and not current_has_source
        ):
            packages.append((current_name, current_version))

    with open(cargo_lock, encoding="utf-8") as fh:
        for line in fh:
            stripped = line.strip()
            if stripped == "[[package]]":
                finish_package()
                current_name = None
                current_version = None
                current_has_source = False
                continue
            if current_name is None and current_version is None and not stripped:
                continue
            if (
                current_name is None
                and current_version is None
                and stripped.startswith("version = ")
            ):
                continue
            if stripped.startswith("name = "):
                current_name = lock_string_value(stripped, "name")
            elif stripped.startswith("version = "):
                current_version = lock_string_value(stripped, "version")
            elif stripped.startswith("source = "):
                current_has_source = True

    finish_package()
    return packages


def lock_string_value(line: str, key: str) -> str | None:
    prefix = f'{key} = "'
    if not line.startswith(prefix) or not line.endswith('"'):
        return None
    return line[len(prefix) : -1]


def refresh_bazel_lockfiles(source_root: Path) -> None:
    if not (source_root / "MODULE.bazel").is_file():
        return

    env = env_with_common_homebrew_bins()
    run(["just", "bazel-lock-update"], cwd=source_root, env=env)
    run(["just", "bazel-lock-check"], cwd=source_root, env=env)


def env_with_common_homebrew_bins() -> dict[str, str]:
    env = os.environ.copy()
    existing_entries = env.get("PATH", "").split(os.pathsep)
    prepend_entries = [
        str(path)
        for path in (Path("/opt/homebrew/bin"), Path("/usr/local/bin"))
        if path.is_dir()
    ]
    env["PATH"] = os.pathsep.join(
        [
            *prepend_entries,
            *[
                entry
                for entry in existing_entries
                if entry and entry not in prepend_entries
            ],
        ]
    )
    return env


def resolve_codesign_identity(explicit_identity: str | None) -> str:
    if explicit_identity == PLACEHOLDER_CODESIGN_IDENTITY:
        raise RuntimeError(
            "APPLE_CODESIGN_IDENTITY still contains the placeholder value. "
            "Set it to a valid Developer ID Application identity or pass "
            "--codesign-identity."
        )

    if os.environ.get("OAI_CODESIGN_BACKEND") == "akv-pkcs11":
        return explicit_identity or "akv-pkcs11"

    identities = native_codesign_identities()
    if explicit_identity:
        ensure_codesign_identity_ready(explicit_identity, identities)
        return explicit_identity

    developer_id_identities = sorted(
        identity
        for identity in identities
        if identity.startswith(DEVELOPER_ID_APPLICATION_PREFIX)
    )
    if len(developer_id_identities) == 1:
        return developer_id_identities[0]
    if not developer_id_identities:
        raise RuntimeError(
            "No Developer ID Application codesign identity was found. "
            "Run `security find-identity -v -p codesigning` to list valid "
            "identities, then set APPLE_CODESIGN_IDENTITY or pass --codesign-identity."
        )

    choices = "\n".join(f"  - {identity}" for identity in developer_id_identities)
    raise RuntimeError(
        "Multiple Developer ID Application codesign identities were found. "
        "Set APPLE_CODESIGN_IDENTITY or pass --codesign-identity with one of:\n"
        f"{choices}"
    )


def ensure_codesign_identity_ready(identity: str, identities: set[str]) -> None:
    if identity not in identities:
        raise RuntimeError(
            f"No native codesign identity named {identity!r} was found. "
            "Run `security find-identity -v -p codesigning` to list valid "
            "identities, then set APPLE_CODESIGN_IDENTITY or pass --codesign-identity."
        )


def native_codesign_identities() -> set[str]:
    try:
        stdout = subprocess.check_output(
            ["security", "find-identity", "-v", "-p", "codesigning"],
            text=True,
        )
    except FileNotFoundError as err:
        raise RuntimeError(
            "The macOS `security` command was not found; native codesign "
            "identity preflight can only run on macOS."
        ) from err
    except subprocess.CalledProcessError as err:
        raise RuntimeError("Could not list native codesign identities.") from err

    identities: set[str] = set()
    for line in stdout.splitlines():
        match = re.match(r'^\s*\d+\)\s+([0-9A-Fa-f]+)\s+"(.+)"$', line)
        if match is not None:
            identities.add(match.group(1))
            identities.add(match.group(2))
    return identities


def default_cargo_build_jobs() -> int:
    return first_positive_int(
        sysctl_int("hw.perflevel0.physicalcpu"),
        sysctl_int("hw.physicalcpu"),
        sysctl_int("hw.logicalcpu"),
        os.cpu_count(),
    )


def first_positive_int(*values: int | None) -> int:
    for value in values:
        if value is not None and value > 0:
            return value
    return 1


def sysctl_int(name: str) -> int | None:
    try:
        stdout = subprocess.check_output(
            ["sysctl", "-n", name],
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return None

    try:
        value = int(stdout.strip())
    except ValueError:
        return None
    return value if value > 0 else None


def resolve_base_version(
    cargo_toml: Path,
    *,
    ls_remote_stdout: str | None,
) -> str:
    cargo_version = read_workspace_version(cargo_toml)
    if cargo_version != WORKSPACE_VERSION_SENTINEL:
        return validate_release_version(cargo_version)

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
            timeout=60,
        )
    return latest_release_version_from_ls_remote(ls_remote_stdout)


def derive_fork_version(
    cargo_version: str,
    *,
    describe_tag: str | None = None,
    ls_remote_stdout: str,
    suffix: str = DEFAULT_SUFFIX,
) -> str:
    _ = describe_tag
    if cargo_version != WORKSPACE_VERSION_SENTINEL:
        base_version = validate_release_version(cargo_version)
    else:
        base_version = latest_release_version_from_ls_remote(ls_remote_stdout)
    return fork_version_from_base(base_version, suffix)


def fork_version_from_base(base_version: str, suffix: str) -> str:
    validate_release_version(base_version)
    if not suffix:
        raise RuntimeError("Version suffix must not be empty.")
    return f"{base_version}-{suffix}"


def github_release_tag(fork_version: str) -> str:
    return f"rust-v{fork_version}"


def publish_github_release(
    *,
    gh: str,
    repo: str,
    tag: str,
    title: str,
    target: str,
    archive_outputs: list[Path],
) -> None:
    if github_release_exists(gh=gh, repo=repo, tag=tag):
        print(f"GitHub release {tag} already exists in {repo}.", flush=True)
    else:
        print(f"Creating GitHub release {tag} in {repo}.", flush=True)
        run(
            [
                gh,
                "release",
                "create",
                tag,
                "--repo",
                repo,
                "--title",
                title,
                "--notes",
                f"apohl79 Codex {title}",
                "--target",
                target,
            ],
            cwd=REPO_ROOT,
        )

    for archive_output in archive_outputs:
        run(
            [
                gh,
                "release",
                "upload",
                tag,
                str(archive_output),
                "--repo",
                repo,
                "--clobber",
            ],
            cwd=REPO_ROOT,
        )


def github_release_exists(*, gh: str, repo: str, tag: str) -> bool:
    result = run(
        [gh, "release", "view", tag, "--repo", repo],
        cwd=REPO_ROOT,
        check=False,
        stdout=subprocess.DEVNULL,
    )
    return result.returncode == 0


def latest_release_version_from_ls_remote(stdout: str) -> str:
    versions = {
        match.group("version")
        for line in stdout.splitlines()
        if (match := LS_REMOTE_TAG_RE.match(line.strip())) is not None
    }
    if not versions:
        raise RuntimeError("No valid upstream rust release tags found.")
    return max(versions, key=release_version_sort_key)


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
    stdout: int | None = None,
) -> subprocess.CompletedProcess:
    print("+ " + shlex.join(command), flush=True)
    try:
        return subprocess.run(command, cwd=cwd, env=env, check=check, stdout=stdout)
    except FileNotFoundError as err:
        raise RuntimeError(f"Command not found: {command[0]}") from err
    except subprocess.CalledProcessError as err:
        raise RuntimeError(
            f"Command failed with exit status {err.returncode}: {shlex.join(command)}"
        ) from err
