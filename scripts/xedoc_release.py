"""Build helpers for Xedoc release packages."""

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import textwrap

from xedoc_package.targets import TARGET_SPECS
from xedoc_package.targets import TargetSpec
from xedoc_package.archive import write_archive
from xedoc_package.model_router_runtime import build_runtime_archive
from xedoc_package.model_router_runtime import runtime_asset_name
from xedoc_package.model_router_runtime import runtime_id
from xedoc_package.model_router_runtime import RuntimeReference
from xedoc_package.remote_agent_runtime import build_remote_agent_runtime


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
DEFAULT_REF = "main"
DEFAULT_GITHUB_REPO = "apohl79/xedoc"
DEFAULT_GITHUB_ACCOUNT = "apohl79"
DEFAULT_BUILD_SYSTEM = "bazel"
CARGO_BUILD_JOBS_ENV_VAR = "XEDOC_CARGO_BUILD_JOBS"
PLACEHOLDER_CODESIGN_IDENTITY = "Developer ID Application: YOUR NAME (TEAMID)"
DEVELOPER_ID_APPLICATION_PREFIX = "Developer ID Application:"
VERSION_RE = re.compile(
    r"^(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.(?P<patch>[0-9]+)"
    r"(?:-(?P<pre_label>alpha|beta)(?:\.(?P<pre_number>[0-9]+))?)?$"
)
RELEASE_TAG_RE = re.compile(
    r"^v[0-9]+\.[0-9]+\.[0-9]+(?:-(?:alpha|beta)(?:\.[0-9]+)?)?$"
)
WORKSPACE_VERSION_LINE_RE = re.compile(r'^(\s*version\s*=\s*)"[^"]+"(.*)$')
BAZEL_RELEASE_CONFIGS = ("buildbuddy-generic-rbe", "xedoc-release")
BAZEL_RELEASE_BUNDLE = "//xedoc-rs:xedoc-release-binaries"
BAZEL_RELEASE_STARTUP_OPTIONS = ["--noexperimental_remote_repo_contents_cache"]
BAZEL_RELEASE_CACHE_OPTIONS = ["--repo_contents_cache="]
DEFAULT_RELEASE_TARGET = "macos-arm64"
RELEASE_TARGET_ALIASES = {
    "macos-arm64": "aarch64-apple-darwin",
    "macos-x86_64": "x86_64-apple-darwin",
    "linux-x86_64": "x86_64-unknown-linux-gnu",
    "linux-arm64": "aarch64-unknown-linux-gnu",
}
RELEASE_TARGET_CHOICES = {
    **RELEASE_TARGET_ALIASES,
    **{target: target for target in RELEASE_TARGET_ALIASES.values()},
}
BAZEL_PLATFORM_BY_TARGET = {
    "aarch64-apple-darwin": "macos_arm64",
    "x86_64-apple-darwin": "macos_amd64",
}
BAZEL_MULTIPLATFORM_TARGET_BY_TARGET = {
    "aarch64-apple-darwin": "//xedoc-rs/cli:xedoc_macos_arm64",
    "x86_64-apple-darwin": "//xedoc-rs/cli:xedoc_macos_amd64",
    "aarch64-unknown-linux-gnu": "//xedoc-rs/cli:xedoc_linux_arm64_gnu.2.28",
    "x86_64-unknown-linux-gnu": "//xedoc-rs/cli:xedoc_linux_amd64_gnu.2.28",
}
BAZEL_MULTIPLATFORM_BWRAP_TARGET_BY_TARGET = {
    "aarch64-unknown-linux-gnu": "//xedoc-rs/bwrap:bwrap_linux_arm64_gnu.2.28",
    "x86_64-unknown-linux-gnu": "//xedoc-rs/bwrap:bwrap_linux_amd64_gnu.2.28",
}


@dataclass(frozen=True)
class ReleaseBinaries:
    entrypoint: Path
    bwrap: Path | None = None


@dataclass(frozen=True)
class ReleasePackage:
    target: str
    package_dir: Path
    archive_outputs: tuple[Path, ...]
    runtime_reference: RuntimeReference
    runtime_archive_output: Path
    remote_agent_runtime_output: Path | None


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a signed local release package for Xedoc.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--ref",
        default=DEFAULT_REF,
        help="Git ref to build. Defaults to the Xedoc main branch.",
    )
    parser.add_argument(
        "--target",
        action="append",
        choices=sorted(RELEASE_TARGET_CHOICES),
        default=argparse.SUPPRESS,
        help=(
            "Release target alias or Rust target triple. May be repeated. "
            f"Defaults to {DEFAULT_RELEASE_TARGET}."
        ),
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
        default=Path("dist/xedoc"),
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
        "--model-router-runtime-archive-output",
        type=Path,
        help=(
            "Archive output for the separately installed semantic model-router "
            "runtime. Its filename must match the immutable runtime ID."
        ),
    )
    parser.add_argument(
        "--remote-agent-runtime-dir",
        type=Path,
        help=(
            "Prebuilt target-pinned remote-agent Python runtime directory. "
            "When omitted, the release flow builds it from the pinned "
            "python-build-standalone distribution."
        ),
    )
    parser.add_argument(
        "--cargo",
        default="cargo",
        help="Cargo executable to use for Cargo builds and lockfile repair.",
    )
    parser.add_argument(
        "--bazel",
        default="bazel",
        help="Bazel executable to use for the release build.",
    )
    parser.add_argument(
        "--build-system",
        choices=("bazel", "cargo"),
        default=DEFAULT_BUILD_SYSTEM,
        help="Build system used to compile release binaries.",
    )
    parser.add_argument(
        "--bazel-build-jobs",
        type=positive_int_arg,
        help=(
            "Maximum CPU resources for Bazel's local action scheduler. Does "
            "not change remote action concurrency."
        ),
    )
    parser.add_argument(
        "--bazel-max-heap-mb",
        type=positive_int_arg,
        help=(
            "Maximum heap size in MiB for the local Bazel server. Does not "
            "change remote executor resources."
        ),
    )
    parser.add_argument(
        "--cargo-build-jobs",
        type=positive_int_arg,
        help=(
            "Maximum parallel Cargo jobs for the release compile. Can also be "
            f"set with {CARGO_BUILD_JOBS_ENV_VAR}; existing "
            "CARGO_BUILD_JOBS is still respected."
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace existing package directory and archive outputs.",
    )
    parser.add_argument(
        "--notarize",
        action="store_true",
        help="Submit the signed packaged macOS binary to Apple notarization.",
    )
    parser.add_argument(
        "--github-repo",
        default=DEFAULT_GITHUB_REPO,
        help="GitHub repository that receives the release and uploaded archives.",
    )
    parser.add_argument(
        "--github-account",
        default=DEFAULT_GITHUB_ACCOUNT,
        help=(
            "Stored gh account whose token is used for publishing when GH_TOKEN "
            "or GITHUB_TOKEN is not already set. Pass an empty value to use the "
            "active gh account."
        ),
    )
    parser.add_argument(
        "--gh",
        default="gh",
        help="GitHub CLI executable used for release publishing.",
    )
    parser.add_argument(
        "--skip-github-release",
        action="store_true",
        help="Build the package without GitHub publication or macOS notarization.",
    )
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help=(
            "Allow local manifest changes. Requires "
            "--skip-github-release because a GitHub release must match its "
            "committed target."
        ),
    )
    parser.add_argument(
        "--keep-worktree",
        action="store_true",
        help=(
            "Deprecated compatibility flag. Release builds now use the current "
            "checkout directly so build caches can be reused."
        ),
    )
    args = parser.parse_args(argv)
    args.targets = resolve_release_targets(getattr(args, "target", None))
    # Keep the historical Namespace shape for callers that build a single target.
    args.target = args.targets[0]
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        build_release(args)
    except RuntimeError as err:
        print(f"Error: {err}", file=sys.stderr)
        return 1
    return 0


def resolve_release_targets(requested_targets: list[str] | None) -> list[str]:
    targets: list[str] = []
    for requested_target in requested_targets or [DEFAULT_RELEASE_TARGET]:
        try:
            target = RELEASE_TARGET_CHOICES[requested_target]
        except KeyError as err:
            supported = ", ".join(sorted(RELEASE_TARGET_ALIASES))
            raise RuntimeError(
                f"Unsupported release target {requested_target!r}. "
                f"Supported aliases: {supported}."
            ) from err
        if target not in targets:
            targets.append(target)
    return targets


def release_targets(args: argparse.Namespace) -> list[str]:
    parsed_targets = getattr(args, "targets", None)
    if parsed_targets is not None:
        return resolve_release_targets(parsed_targets)

    target = getattr(args, "target", None)
    if isinstance(target, list):
        return resolve_release_targets(target)
    return resolve_release_targets([target] if target else None)


def ensure_target_specific_paths_are_unambiguous(
    args: argparse.Namespace,
    targets: list[str],
) -> None:
    if len(targets) == 1:
        return

    options = {
        "--archive-output": getattr(args, "archive_output", []),
        "--model-router-runtime-archive-output": getattr(
            args, "model_router_runtime_archive_output", None
        ),
        "--package-dir": getattr(args, "package_dir", None),
        "--remote-agent-runtime-dir": getattr(args, "remote_agent_runtime_dir", None),
    }
    supplied = [flag for flag, value in options.items() if value]
    if supplied:
        raise RuntimeError(
            f"{', '.join(supplied)} can be used only with a single --target."
        )


def build_release(args: argparse.Namespace) -> None:
    targets = release_targets(args)
    ensure_target_specific_paths_are_unambiguous(args, targets)
    if getattr(args, "allow_dirty", False) and not args.skip_github_release:
        raise RuntimeError(
            "--allow-dirty requires --skip-github-release because a GitHub "
            "release must match its committed target."
        )

    codesign_identity = (
        resolve_codesign_identity(args.codesign_identity)
        if any(target.endswith("apple-darwin") for target in targets)
        else None
    )

    output_dir = resolve_repo_path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    source_root = REPO_ROOT
    cargo_toml = source_root / "xedoc-rs" / "Cargo.toml"
    cargo_lock = source_root / "xedoc-rs" / "Cargo.lock"
    ensure_current_checkout_matches_ref(args.ref)
    if not getattr(args, "allow_dirty", False):
        ensure_git_path_clean(cargo_toml)
        ensure_git_path_clean(cargo_lock)
    version = validate_release_version(read_workspace_version(cargo_toml))
    release_tag = github_release_tag(version)
    release_target = None
    github_env = None
    if not args.skip_github_release:
        release_target = git_commit("HEAD")
        github_env = github_release_env(gh=args.gh, account=args.github_account)
        ensure_github_release_target_exists(
            gh=args.gh,
            repo=args.github_repo,
            target=release_target,
            ref=args.ref,
            env=github_env,
        )

    repair_stale_release_lockfiles(
        cargo=args.cargo,
        source_root=source_root,
        cargo_toml=cargo_toml,
        cargo_lock=cargo_lock,
        target=targets[0],
    )

    build_system = getattr(args, "build_system", DEFAULT_BUILD_SYSTEM)
    if build_system == "bazel":
        built_binaries = build_bazel_release_binaries_for_targets(
            bazel=getattr(args, "bazel", "bazel"),
            bazel_build_jobs=getattr(args, "bazel_build_jobs", None),
            bazel_max_heap_mb=getattr(args, "bazel_max_heap_mb", None),
            source_root=source_root,
            targets=targets,
        )
        release_binaries_by_target = {
            target: stage_release_binaries(
                built_binaries[target],
                output_dir / ".bazel-release" / version / target,
            )
            for target in targets
        }
    elif build_system == "cargo":
        release_binaries_by_target = {
            target: build_cargo_release_binaries(
                cargo=args.cargo,
                cargo_build_jobs=getattr(args, "cargo_build_jobs", None),
                source_root=source_root,
                spec=TARGET_SPECS[target],
                target=target,
            )
            for target in targets
        }
    else:
        raise RuntimeError(f"Unsupported release build system: {build_system}")

    release_packages = [
        package_release_target(
            args=args,
            codesign_identity=codesign_identity,
            output_dir=output_dir,
            release_binaries=release_binaries_by_target[target],
            source_root=source_root,
            target=target,
            version=version,
        )
        for target in targets
    ]

    if not args.skip_github_release:
        for package in release_packages:
            publish_immutable_runtime_release(
                gh=args.gh,
                repo=args.github_repo,
                reference=package.runtime_reference,
                target=release_target,
                archive_output=package.runtime_archive_output,
                env=github_env,
            )
        publish_github_release(
            gh=args.gh,
            repo=args.github_repo,
            tag=release_tag,
            title=version,
            target=release_target,
            archive_outputs=[
                archive_output
                for package in release_packages
                for archive_output in package.archive_outputs
            ],
            env=github_env,
            notes=generate_release_notes(
                release_tag,
                version,
                gh=args.gh,
                repo=args.github_repo,
                env=github_env,
                target=release_target,
            ),
        )

    print(f"Built Xedoc release {version}")
    if not args.skip_github_release:
        print(f"GitHub release: {release_tag}")
    for package in release_packages:
        print(f"Package directory ({package.target}): {package.package_dir}")
        for archive_output in package.archive_outputs:
            print(f"Archive ({package.target}): {archive_output}")
        print(
            f"Model-router runtime archive ({package.target}): "
            f"{package.runtime_archive_output}"
        )
        if package.remote_agent_runtime_output is not None:
            print(
                f"Remote-agent runtime ({package.target}): "
                f"{package.remote_agent_runtime_output}"
            )


def package_release_target(
    *,
    args: argparse.Namespace,
    codesign_identity: str | None,
    output_dir: Path,
    release_binaries: ReleaseBinaries,
    source_root: Path,
    target: str,
    version: str,
) -> ReleasePackage:
    spec = TARGET_SPECS[target]
    signing_script = source_root / ".github/scripts/macos-signing/sign_macos_code.sh"
    entitlements = (
        source_root / ".github/scripts/macos-signing/xedoc.entitlements.plist"
    )

    explicit_package_dir = getattr(args, "package_dir", None)
    package_dir = (
        resolve_repo_path(explicit_package_dir)
        if explicit_package_dir is not None
        else output_dir / version / f"xedoc-package-{target}"
    )
    archive_outputs = [
        resolve_repo_path(path) for path in getattr(args, "archive_output", [])
    ] or [output_dir / version / f"xedoc-{target}-{version}.zip"]
    runtime_id_value = runtime_id()
    explicit_runtime_archive_output = getattr(
        args, "model_router_runtime_archive_output", None
    )
    runtime_archive_output = (
        resolve_repo_path(explicit_runtime_archive_output)
        if explicit_runtime_archive_output is not None
        else output_dir
        / "model-router-runtime"
        / runtime_id_value
        / runtime_asset_name(runtime_id_value, target)
    )
    runtime_reference = build_runtime_archive(
        spec,
        runtime_archive_output,
        force=args.force,
    )
    remote_agent_runtime = None
    remote_agent_runtime_output = None
    if not spec.is_windows:
        remote_agent_runtime_output = (
            output_dir / "remote-agent-runtime" / version / target
        )
        remote_agent_runtime_source = (
            resolve_repo_path(getattr(args, "remote_agent_runtime_dir", None))
            if getattr(args, "remote_agent_runtime_dir", None) is not None
            else None
        )
        remote_agent_runtime = build_remote_agent_runtime(
            spec,
            remote_agent_runtime_output,
            force=args.force,
            source=remote_agent_runtime_source,
        )

    package_args = [
        sys.executable,
        str(source_root / "scripts/build_xedoc_package.py"),
        "--target",
        target,
        "--variant",
        "xedoc",
        "--version",
        version,
        "--entrypoint-bin",
        str(release_binaries.entrypoint),
        "--cargo-profile",
        "release",
        "--package-dir",
        str(package_dir),
        "--include-session-control",
        "--model-router-runtime-id",
        runtime_reference.runtime_id,
        "--model-router-runtime-asset",
        runtime_reference.asset_name,
        "--model-router-runtime-sha256",
        runtime_reference.sha256,
        "--model-router-runtime-source-release-tag",
        runtime_reference.source_release_tag,
    ]
    if remote_agent_runtime is not None:
        package_args.extend(
            ["--remote-agent-runtime-dir", str(remote_agent_runtime.root.parent)]
        )
    if release_binaries.bwrap is not None:
        package_args.extend(["--bwrap-bin", str(release_binaries.bwrap)])
    if args.force:
        package_args.append("--force")

    run(package_args, cwd=source_root)
    packaged_entrypoint = package_dir / "bin" / "xedoc"
    if target.endswith("apple-darwin"):
        if codesign_identity is None:
            raise RuntimeError(f"Missing codesign identity for macOS target {target}.")
        run(
            build_codesign_command(
                target=packaged_entrypoint,
                identity=codesign_identity,
                entitlements=entitlements,
                signing_script=signing_script,
            ),
            cwd=source_root,
        )
        run(
            [
                "codesign",
                "--verify",
                "--strict",
                "--verbose=2",
                str(packaged_entrypoint),
            ]
        )
        if getattr(args, "notarize", False) and not args.skip_github_release:
            run(
                [
                    str(
                        source_root
                        / ".github/scripts/macos-signing/notarize_macos_binary_with_rcodesign.sh"
                    ),
                    "--binary",
                    str(packaged_entrypoint),
                    "--report-dir",
                    str(output_dir / version / "notarization" / target),
                ],
                cwd=source_root,
            )
    for archive_output in archive_outputs:
        write_archive(package_dir, archive_output, force=args.force)

    return ReleasePackage(
        target=target,
        package_dir=package_dir,
        archive_outputs=tuple(archive_outputs),
        runtime_reference=runtime_reference,
        runtime_archive_output=runtime_archive_output,
        remote_agent_runtime_output=remote_agent_runtime_output,
    )


def build_cargo_release_binaries(
    *,
    cargo: str,
    cargo_build_jobs: int | None,
    source_root: Path,
    spec: TargetSpec,
    target: str,
) -> ReleaseBinaries:
    target_dir = Path(
        os.environ.get("CARGO_TARGET_DIR", source_root / "xedoc-rs" / "target")
    ).resolve()
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    resolved_cargo_build_jobs = resolve_cargo_build_jobs(cargo_build_jobs)
    if resolved_cargo_build_jobs is not None:
        env["CARGO_BUILD_JOBS"] = resolved_cargo_build_jobs
    else:
        env.setdefault("CARGO_BUILD_JOBS", str(default_cargo_build_jobs()))
    env.setdefault("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO", "packed")
    env.setdefault("CARGO_NET_GIT_FETCH_WITH_CLI", "true")

    run(
        [
            cargo,
            "build",
            "--locked",
            "--manifest-path",
            str(source_root / "xedoc-rs" / "Cargo.toml"),
            "--package",
            "xedoc-cli",
            "--bins",
            "--profile",
            "release",
            "--target",
            target,
        ],
        cwd=source_root / "xedoc-rs",
        env=env,
    )

    return ReleaseBinaries(
        entrypoint=require_built_file(
            target_dir / spec.target / "release" / f"xedoc{spec.exe_suffix}",
            "Built entrypoint",
        ),
    )


def build_bazel_release_binaries_for_targets(
    *,
    bazel: str,
    bazel_build_jobs: int | None = None,
    bazel_max_heap_mb: int | None = None,
    source_root: Path,
    targets: list[str],
) -> dict[str, ReleaseBinaries]:
    target_labels = [bazel_multiplatform_release_target(target) for target in targets]
    bwrap_labels = {
        target: bazel_multiplatform_bwrap_target(target)
        for target in targets
        if TARGET_SPECS[target].is_linux
    }
    startup_options = [
        *BAZEL_RELEASE_STARTUP_OPTIONS,
        *(
            [f"--host_jvm_args=-Xmx{bazel_max_heap_mb}m"]
            if bazel_max_heap_mb is not None
            else []
        ),
    ]
    local_build_options = (
        [f"--local_resources=cpu={bazel_build_jobs}"]
        if bazel_build_jobs is not None
        else []
    )
    options = [f"--config={config}" for config in BAZEL_RELEASE_CONFIGS]
    run(
        [
            bazel,
            *startup_options,
            "build",
            *BAZEL_RELEASE_CACHE_OPTIONS,
            *options,
            *local_build_options,
            "--",
            *target_labels,
            *bwrap_labels.values(),
        ],
        cwd=source_root,
    )
    execution_root = bazel_execution_root(
        command_output(
            [
                bazel,
                *startup_options,
                "info",
                *BAZEL_RELEASE_CACHE_OPTIONS,
                "execution_root",
            ],
            cwd=source_root,
        )
    )
    release_binaries: dict[str, ReleaseBinaries] = {}
    for target, target_label in zip(targets, target_labels, strict=True):
        entrypoint = resolve_bazel_multiplatform_binary(
            command_output(
                [
                    bazel,
                    *startup_options,
                    "cquery",
                    *BAZEL_RELEASE_CACHE_OPTIONS,
                    *options,
                    "--output=files",
                    "--",
                    target_label,
                ],
                cwd=source_root,
            ),
            execution_root,
            binary_name="xedoc",
            description="Bazel entrypoint",
        )
        bwrap_label = bwrap_labels.get(target)
        bwrap = (
            resolve_bazel_multiplatform_binary(
                command_output(
                    [
                        bazel,
                        *startup_options,
                        "cquery",
                        *BAZEL_RELEASE_CACHE_OPTIONS,
                        *options,
                        "--output=files",
                        "--",
                        bwrap_label,
                    ],
                    cwd=source_root,
                ),
                execution_root,
                binary_name="bwrap",
                description="Bazel bwrap",
            )
            if bwrap_label is not None
            else None
        )
        release_binaries[target] = ReleaseBinaries(
            entrypoint=entrypoint,
            bwrap=bwrap,
        )
    return release_binaries


def build_bazel_release_binaries(
    *,
    bazel: str,
    bazel_build_jobs: int | None = None,
    bazel_max_heap_mb: int | None = None,
    source_root: Path,
    target: str,
) -> ReleaseBinaries:
    options = bazel_release_options(target)
    startup_options = [
        *BAZEL_RELEASE_STARTUP_OPTIONS,
        *(
            [f"--host_jvm_args=-Xmx{bazel_max_heap_mb}m"]
            if bazel_max_heap_mb is not None
            else []
        ),
    ]
    local_build_options = (
        [f"--local_resources=cpu={bazel_build_jobs}"]
        if bazel_build_jobs is not None
        else []
    )
    run(
        [
            bazel,
            *startup_options,
            "build",
            *BAZEL_RELEASE_CACHE_OPTIONS,
            *options,
            *local_build_options,
            "--",
            BAZEL_RELEASE_BUNDLE,
        ],
        cwd=source_root,
    )
    execution_root = bazel_execution_root(
        command_output(
            [
                bazel,
                *startup_options,
                "info",
                *BAZEL_RELEASE_CACHE_OPTIONS,
                "execution_root",
            ],
            cwd=source_root,
        )
    )
    outputs = command_output(
        [
            bazel,
            *startup_options,
            "cquery",
            *BAZEL_RELEASE_CACHE_OPTIONS,
            *options,
            "--output=files",
            "--",
            BAZEL_RELEASE_BUNDLE,
        ],
        cwd=source_root,
    )
    return resolve_bazel_release_binaries(outputs, execution_root)


def bazel_release_options(target: str) -> list[str]:
    try:
        platform = BAZEL_PLATFORM_BY_TARGET[target]
    except KeyError as err:
        raise RuntimeError(f"No Bazel release platform for target {target}.") from err
    return [
        *(f"--config={config}" for config in BAZEL_RELEASE_CONFIGS),
        f"--platforms=@llvm//platforms:{platform}",
    ]


def bazel_multiplatform_release_target(target: str) -> str:
    try:
        return BAZEL_MULTIPLATFORM_TARGET_BY_TARGET[target]
    except KeyError as err:
        raise RuntimeError(
            f"No Bazel multi-platform release target for {target}."
        ) from err


def bazel_multiplatform_bwrap_target(target: str) -> str:
    try:
        return BAZEL_MULTIPLATFORM_BWRAP_TARGET_BY_TARGET[target]
    except KeyError as err:
        raise RuntimeError(
            f"No Bazel multi-platform bwrap target for {target}."
        ) from err


def bazel_execution_root(stdout: str) -> Path:
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    if len(lines) != 1 or not Path(lines[0]).is_absolute():
        raise RuntimeError("Bazel did not report one absolute execution root.")
    return Path(lines[0])


def resolve_bazel_release_binaries(
    stdout: str,
    execution_root: Path,
) -> ReleaseBinaries:
    outputs = [line.strip() for line in stdout.splitlines() if line.strip()]
    paths = [execution_root / output for output in outputs]
    paths_by_name = {path.name: path for path in paths}
    if len(outputs) != 1 or paths_by_name.keys() != {"xedoc"}:
        raise RuntimeError(
            "Bazel release bundle must contain exactly xedoc; "
            f"reported {sorted(paths_by_name)}."
        )
    return ReleaseBinaries(
        entrypoint=require_built_file(paths_by_name["xedoc"], "Bazel entrypoint"),
    )


def resolve_bazel_multiplatform_release_binary(
    stdout: str,
    execution_root: Path,
) -> ReleaseBinaries:
    return ReleaseBinaries(
        entrypoint=resolve_bazel_multiplatform_binary(
            stdout,
            execution_root,
            binary_name="xedoc",
            description="Bazel entrypoint",
        )
    )


def resolve_bazel_multiplatform_binary(
    stdout: str,
    execution_root: Path,
    *,
    binary_name: str,
    description: str,
) -> Path:
    paths = [
        execution_root / output
        for output in (line.strip() for line in stdout.splitlines())
        if output
    ]
    binaries = [path for path in paths if path.name == binary_name]
    if len(binaries) != 1:
        raise RuntimeError(
            "Bazel multi-platform release target must contain exactly one "
            f"{binary_name} binary; reported {[path.name for path in paths]}."
        )
    return require_built_file(binaries[0], description)


def stage_release_binaries(
    release_binaries: ReleaseBinaries,
    staging_dir: Path,
) -> ReleaseBinaries:
    staging_dir.mkdir(parents=True, exist_ok=True)
    entrypoint = staging_dir / release_binaries.entrypoint.name
    # Bazel outputs are read-only and copy2 keeps that mode, so drop any
    # previously staged binary before overwriting it.
    entrypoint.unlink(missing_ok=True)
    shutil.copy2(release_binaries.entrypoint, entrypoint)
    bwrap = None
    if release_binaries.bwrap is not None:
        bwrap = staging_dir / release_binaries.bwrap.name
        bwrap.unlink(missing_ok=True)
        shutil.copy2(release_binaries.bwrap, bwrap)
        bwrap = bwrap.resolve()
    return ReleaseBinaries(entrypoint=entrypoint.resolve(), bwrap=bwrap)


def require_built_file(path: Path, description: str) -> Path:
    path = path.resolve()
    if not path.is_file():
        raise RuntimeError(f"{description} not found: {path}")
    return path


def ensure_current_checkout_matches_ref(ref: str) -> None:
    head_commit = git_commit("HEAD")
    ref_commit = git_commit(ref)
    if head_commit != ref_commit:
        raise RuntimeError(
            f"Current checkout HEAD ({head_commit[:12]}) does not match "
            f"--ref {ref} ({ref_commit[:12]}). Check out {ref} before running "
            "the incremental Xedoc release build."
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
                "running the Xedoc release build."
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
        cwd=source_root / "xedoc-rs",
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


def resolve_cargo_build_jobs(explicit_jobs: int | None) -> str | None:
    if explicit_jobs is not None:
        return str(explicit_jobs)

    env_jobs = os.environ.get(CARGO_BUILD_JOBS_ENV_VAR)
    if env_jobs is None:
        return None

    return str(positive_int_env(CARGO_BUILD_JOBS_ENV_VAR, env_jobs))


def positive_int_arg(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as err:
        raise argparse.ArgumentTypeError("must be a positive integer") from err

    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")

    return parsed


def positive_int_env(name: str, value: str) -> int:
    try:
        return positive_int_arg(value)
    except argparse.ArgumentTypeError as err:
        raise RuntimeError(f"{name} must be a positive integer.") from err


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


def github_release_tag(version: str) -> str:
    return f"v{version}"


def is_release_tag(tag: str) -> bool:
    return RELEASE_TAG_RE.match(tag) is not None


def ensure_github_release_target_exists(
    *,
    gh: str,
    repo: str,
    target: str,
    ref: str,
    env: dict[str, str] | None = None,
) -> None:
    result = run(
        [gh, "api", f"repos/{repo}/commits/{target}"],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        stdout=subprocess.DEVNULL,
    )
    if result.returncode == 0:
        return

    raise RuntimeError(
        f"Release target commit {target[:12]} for --ref {ref} is not available "
        f"in GitHub repository {repo}. Push {ref} to {repo} before publishing, "
        "verify `gh` can access that repository, or rerun with "
        "--skip-github-release to build the package locally."
    )


def generate_release_notes(
    tag: str,
    version: str,
    *,
    gh: str = "gh",
    repo: str = DEFAULT_GITHUB_REPO,
    env: dict[str, str] | None = None,
    target: str = "HEAD",
) -> str:
    """Generate a changelog body for the GitHub release from git history."""
    if not _is_git_repo():
        return f"Xedoc {version}"

    previous_release = find_previous_published_release(tag, gh=gh, repo=repo, env=env)
    date = subprocess.check_output(
        ["git", "log", "-1", "--format=%ad", "--date=format:%Y-%m-%d", target],
        cwd=REPO_ROOT,
        text=True,
    ).strip()

    if previous_release is None:
        return initial_release_notes(date, version)

    _prev_tag, previous_target = previous_release
    return incremental_release_notes(
        version=version,
        date=date,
        commits=release_commits_between(previous_target, target),
    )


def find_previous_release_tag(tag: str) -> str | None:
    """Return the most recent Xedoc release tag before *tag*, or None."""
    try:
        all_tags = (
            subprocess.check_output(
                ["git", "tag", "--sort=creatordate"],
                cwd=REPO_ROOT,
                text=True,
            )
            .strip()
            .split("\n")
        )
    except subprocess.CalledProcessError:
        return None

    release_tags = [t for t in all_tags if is_release_tag(t)]
    try:
        idx = release_tags.index(tag)
    except ValueError:
        return release_tags[-1] if release_tags else None
    return release_tags[idx - 1] if idx > 0 else None


def find_previous_published_release(
    tag: str,
    *,
    gh: str,
    repo: str,
    env: dict[str, str] | None,
) -> tuple[str, str] | None:
    """Return the previous published release tag and target commit."""
    try:
        releases = json.loads(
            subprocess.check_output(
                [
                    gh,
                    "api",
                    f"repos/{repo}/releases",
                    "--paginate",
                ],
                cwd=REPO_ROOT,
                env={**os.environ, **env} if env else None,
                text=True,
            )
        )
    except (FileNotFoundError, subprocess.CalledProcessError, json.JSONDecodeError):
        previous_tag = find_previous_release_tag(tag)
        return (previous_tag, previous_tag) if previous_tag else None

    for release in releases:
        previous_tag = release["tag_name"]
        if previous_tag != tag and is_release_tag(previous_tag):
            return previous_tag, release["target_commitish"]
    return None


def _is_git_repo() -> bool:
    """Return True if REPO_ROOT is inside a git working tree."""
    try:
        subprocess.check_output(
            ["git", "rev-parse", "--is-inside-work-tree"],
            cwd=REPO_ROOT,
            text=True,
            stderr=subprocess.DEVNULL,
        )
        return True
    except (subprocess.CalledProcessError, FileNotFoundError):
        return False


RELEASE_AUTHOR_ENV_VAR = "XEDOC_RELEASE_AUTHOR"


def release_author(prev_tag: str) -> str:
    """Return the author pattern used to identify Xedoc commits.

    Prefers the XEDOC_RELEASE_AUTHOR environment variable, then auto-detects
    from the author of the previous release's tip commit, and falls back to a
    hard-coded default when neither is available. Upstream Codex commits merged
    into the tree are excluded this way.
    """
    env_author = os.environ.get(RELEASE_AUTHOR_ENV_VAR)
    if env_author:
        return env_author
    try:
        return subprocess.check_output(
            ["git", "log", "-1", "--format=%an", prev_tag],
            cwd=REPO_ROOT,
            text=True,
        ).strip()
    except subprocess.CalledProcessError:
        return "Andreas Pohl"


def release_commits_between(prev_tag: str, ref: str) -> list[str]:
    """Return Xedoc commit subjects between *prev_tag* and *ref*.

    Filters by commit author so upstream commits are excluded without a
    manually maintained keyword list.
    """
    author = release_author(prev_tag)
    try:
        raw = subprocess.check_output(
            [
                "git",
                "log",
                "--oneline",
                "--no-merges",
                "--author",
                author,
                f"{prev_tag}..{ref}",
            ],
            cwd=REPO_ROOT,
            text=True,
        )
    except subprocess.CalledProcessError:
        return []
    lines = [ln.strip() for ln in raw.split("\n") if ln.strip()]
    return [ln.split(maxsplit=1)[1] for ln in lines]


def _bullet_list(items: list[str], indent: str = "") -> str:
    if not items:
        return ""
    return "\n".join(f"{indent}- {item}" for item in items)


def initial_release_notes(date: str, version: str) -> str:
    return textwrap.dedent(f"""\
        ## Xedoc {version}

        **Release date:** {date}

        ### Initial Release

        This is the initial Xedoc release.""")


def incremental_release_notes(
    *,
    version: str,
    date: str,
    commits: list[str],
) -> str:
    header = textwrap.dedent(f"""\
        ## Xedoc {version}

        **Release date:** {date}""")

    if not commits:
        return header

    sections: list[str] = [header]

    features: list[str] = []
    fixes: list[str] = []
    other: list[str] = []

    for msg in commits:
        lower = msg.lower()
        if lower.startswith("fix") or lower.startswith("hotfix"):
            fixes.append(msg)
        elif lower.startswith("feat") or any(
            kw in lower for kw in ["add ", "support ", "introduce", "implement"]
        ):
            features.append(msg)
        else:
            other.append(msg)

    if features:
        sections.append("")
        sections.append("### Features")
        sections.append("")
        sections.append(_bullet_list(features))

    if fixes:
        sections.append("")
        sections.append("### Fixes")
        sections.append("")
        sections.append(_bullet_list(fixes))

    if other and not features and not fixes:
        sections.append("")
        sections.append("### Changes")
        sections.append("")
        sections.append(_bullet_list(other))

    return "\n".join(sections)


def publish_github_release(
    *,
    gh: str,
    repo: str,
    tag: str,
    title: str,
    target: str,
    archive_outputs: list[Path],
    env: dict[str, str] | None = None,
    notes: str | None = None,
) -> None:
    if github_release_exists(gh=gh, repo=repo, tag=tag, env=env):
        print(f"GitHub release {tag} already exists in {repo}.", flush=True)
    else:
        print(f"Creating GitHub release {tag} in {repo}.", flush=True)
        if notes is None:
            notes = f"Xedoc {title}"
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
                notes,
                "--target",
                target,
            ],
            cwd=REPO_ROOT,
            env=env,
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
            env=env,
        )


def publish_immutable_runtime_release(
    *,
    gh: str,
    repo: str,
    reference: RuntimeReference,
    target: str,
    archive_output: Path,
    env: dict[str, str] | None = None,
) -> None:
    """Publish a runtime once, refusing to alter an existing immutable release."""
    tag = reference.source_release_tag
    if github_release_exists(gh=gh, repo=repo, tag=tag, env=env):
        digest = github_release_asset_digest(
            gh=gh,
            repo=repo,
            tag=tag,
            asset_name=reference.asset_name,
            env=env,
        )
        if digest is None:
            print(
                f"Adding {reference.asset_name} to immutable model-router runtime "
                f"release {tag} in {repo}.",
                flush=True,
            )
            run(
                [
                    gh,
                    "release",
                    "upload",
                    tag,
                    str(archive_output),
                    "--repo",
                    repo,
                ],
                cwd=REPO_ROOT,
                env=env,
            )
        elif digest != reference.sha256:
            raise RuntimeError(
                f"Immutable runtime release {tag} already has a different "
                f"{reference.asset_name} digest: expected {reference.sha256}, got {digest}."
            )
        else:
            print(
                f"Reusing immutable model-router runtime release {tag} in {repo}.",
                flush=True,
            )
            return
        published_digest = github_release_asset_digest(
            gh=gh,
            repo=repo,
            tag=tag,
            asset_name=reference.asset_name,
            env=env,
        )
        if published_digest != reference.sha256:
            raise RuntimeError(
                f"Published runtime asset {reference.asset_name} digest mismatch: "
                f"expected {reference.sha256}, got {published_digest}."
            )
        return

    print(f"Creating immutable model-router runtime release {tag} in {repo}.")
    run(
        [
            gh,
            "release",
            "create",
            tag,
            "--repo",
            repo,
            "--title",
            f"Model router runtime {reference.runtime_id}",
            "--notes",
            (f"Immutable semantic model-router runtime. SHA-256: {reference.sha256}"),
            "--target",
            target,
        ],
        cwd=REPO_ROOT,
        env=env,
    )
    run(
        [
            gh,
            "release",
            "upload",
            tag,
            str(archive_output),
            "--repo",
            repo,
        ],
        cwd=REPO_ROOT,
        env=env,
    )
    published_digest = github_release_asset_digest(
        gh=gh,
        repo=repo,
        tag=tag,
        asset_name=reference.asset_name,
        env=env,
    )
    if published_digest != reference.sha256:
        raise RuntimeError(
            f"Published runtime asset {reference.asset_name} digest mismatch: "
            f"expected {reference.sha256}, got {published_digest}."
        )


def github_release_asset_digest(
    *,
    gh: str,
    repo: str,
    tag: str,
    asset_name: str,
    env: dict[str, str] | None = None,
) -> str | None:
    release = json.loads(
        command_output(
            [gh, "api", f"repos/{repo}/releases/tags/{tag}"],
            cwd=REPO_ROOT,
            env=env,
        )
    )
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise RuntimeError(f"GitHub runtime release {tag} has invalid asset metadata.")
    for asset in assets:
        if not isinstance(asset, dict) or asset.get("name") != asset_name:
            continue
        digest = asset.get("digest")
        if isinstance(digest, str) and digest.startswith("sha256:"):
            return digest.removeprefix("sha256:")
        raise RuntimeError(
            f"GitHub runtime release {tag} does not expose a SHA-256 digest for "
            f"{asset_name}."
        )
    return None


def github_release_exists(
    *,
    gh: str,
    repo: str,
    tag: str,
    env: dict[str, str] | None = None,
) -> bool:
    result = run(
        [gh, "release", "view", tag, "--repo", repo],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        stdout=subprocess.DEVNULL,
    )
    return result.returncode == 0


def github_release_env(*, gh: str, account: str | None) -> dict[str, str] | None:
    if os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN") or not account:
        return None

    try:
        token = subprocess.check_output(
            [gh, "auth", "token", "-h", "github.com", "-u", account],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (FileNotFoundError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"Could not read gh auth token for GitHub account {account!r}. "
            "Run `gh auth status -h github.com` or pass --github-account ''."
        ) from err

    if not token:
        raise RuntimeError(
            f"gh returned an empty token for GitHub account {account!r}."
        )

    env = os.environ.copy()
    env["GH_TOKEN"] = token
    return env


def validate_release_version(version: str) -> str:
    if VERSION_RE.match(version) is None:
        raise RuntimeError(
            f"Invalid Xedoc release version: {version}. Expected x.y.z[-alpha[.N]|-beta[.N]]."
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
        "xedoc",
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


def command_output(
    command: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> str:
    print("+ " + shlex.join(command), flush=True)
    try:
        return subprocess.check_output(command, cwd=cwd, env=env, text=True)
    except FileNotFoundError as err:
        raise RuntimeError(f"Command not found: {command[0]}") from err
    except subprocess.CalledProcessError as err:
        raise RuntimeError(
            f"Command failed with exit status {err.returncode}: {shlex.join(command)}"
        ) from err


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
