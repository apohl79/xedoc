"""Validate selected release stages before compiling release binaries."""

import argparse
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

from xedoc_desktop_release import DESKTOP_BUILDS, desktop_asset_name
from xedoc_package.archive import archive_format_for_path, resolve_zstd_command
from xedoc_package.dotslash import artifact_for_target
from xedoc_package.model_router_runtime import runtime_asset_name, runtime_id
from xedoc_package.remote_agent_runtime import (
    load_dependency_lock,
    runtime_interpreter_path,
)
from xedoc_package.targets import TARGET_SPECS
from xedoc_release_retry import run_with_retry


def preflight_release(
    args: argparse.Namespace,
    *,
    source_root: Path,
    output_dir: Path,
    version: str,
    targets: list[str],
    include_ios: bool,
    include_macos: bool,
    include_desktop: bool,
    codesign_identity: str | None,
    github_env: dict[str, str] | None,
) -> None:
    tools: set[str] = set()
    resources: list[Path] = []
    outputs: list[tuple[Path, str]] = []
    if targets:
        tools.add(args.bazel if args.build_system == "bazel" else args.cargo)
        if (
            args.build_system == "bazel"
            and Path(args.bazel).resolve()
            == (source_root / ".github/scripts/run_bazel_with_buildbuddy.py").resolve()
        ):
            tools.add(os.environ.get("XEDOC_BAZEL_BIN", "bazel"))
        resources.extend(
            source_root / name
            for name in (
                "scripts/build_xedoc_package.py",
                "scripts/xedoc-session",
                "scripts/session_script_sdk.py",
                "scripts/model-router/reference-router",
                "scripts/model-router/reference-router-embedder.py",
                "scripts/model-router/reference-router.policy.json",
                "xedoc-rs/models-manager/catalog.json",
                "remote-agent/remote-tools.json",
                "remote-agent/__main__.py",
            )
        )
    if include_ios:
        tools.update(
            ("xcodegen", "xcodebuild", "xcrun", "codesign", "ditto", "security")
        )
        resources.extend(
            (
                source_root / "scripts/install/install_ios.py",
                source_root / "ios/project.yml",
            )
        )
        outputs.append((output_dir / version / f"xedoc-ios-{version}.zip", "archive"))
    if include_macos:
        tools.update(("codesign", "ditto", "lipo", "xcrun", "spctl"))
        outputs.append((output_dir / version / f"xedoc-macos-{version}.zip", "archive"))
    if include_desktop:
        outputs.extend(
            (
                output_dir / version / desktop_asset_name(version, arch, extension),
                "file",
            )
            for _, _, _, arch, extension in DESKTOP_BUILDS
        )
    if include_macos or include_desktop:
        tools.add("corepack" if shutil.which("corepack") else "pnpm")
    if include_macos or include_desktop:
        resources.extend(
            source_root / name
            for name in ("package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml")
        )
    macos_cli = any(target.endswith("apple-darwin") for target in targets)
    signing_dir = source_root / ".github/scripts/macos-signing"
    if include_macos or macos_cli:
        tools.add("codesign")
        resources.extend(
            signing_dir / name
            for name in ("sign_macos_code.sh", "xedoc.entitlements.plist")
        )
    for tool in sorted(tools):
        if shutil.which(tool) is None:
            raise RuntimeError(f"Release preflight requires executable: {tool}")
    for resource in resources:
        if not resource.is_file():
            raise RuntimeError(f"Missing release resource: {resource}")
    if not args.skip_github_release:
        repositories = [args.github_repo]
        if include_macos and "-" not in version and args.github_repo == "apohl79/xedoc":
            repositories.append("apohl79/homebrew-xedoc")
        for repository in repositories:
            try:
                result = run_with_retry(
                    [
                        args.gh,
                        "api",
                        f"repos/{repository}",
                        "--jq",
                        ".permissions.push",
                    ],
                    env=github_env,
                    capture_output=True,
                )
            except (OSError, subprocess.CalledProcessError) as error:
                raise RuntimeError(
                    f"GitHub access preflight failed for {repository}."
                ) from error
            if result.stdout.strip() == "false":
                raise RuntimeError(f"GitHub credentials cannot write to {repository}.")
    for target in targets:
        spec = TARGET_SPECS[target]
        runtime_id_value = runtime_id(target, codesign_identity)
        load_dependency_lock(spec)
        artifact_for_target(
            spec, source_root / "scripts/xedoc_package/rg", artifact_label="ripgrep"
        )
        package_dir = args.package_dir or (
            output_dir / version / f"xedoc-package-{target}"
        )
        outputs.append((package_dir, "package"))
        outputs.extend(
            (path, "archive")
            for path in (
                args.archive_output
                or [output_dir / version / f"xedoc-{target}-{version}.zip"]
            )
        )
        asset_name = runtime_asset_name(runtime_id_value, target)
        runtime_archive = args.model_router_runtime_archive_output or (
            output_dir / "model-router-runtime" / runtime_id_value / asset_name
        )
        if runtime_archive.name != asset_name:
            raise RuntimeError(
                "Runtime archive name must match its immutable identity: "
                f"expected {asset_name}, got {runtime_archive.name}"
            )
        outputs.append(
            (runtime_archive, "archive" if args.skip_github_release else "cached")
        )
        outputs.append(
            (output_dir / "remote-agent-runtime" / version / target, "directory")
        )
        if args.remote_agent_runtime_dir is not None:
            source = args.remote_agent_runtime_dir
            if not source.is_absolute():
                source = source_root / source
            if not any(
                runtime_interpreter_path(candidate, spec).is_file()
                for candidate in (source, source / "python")
            ):
                raise RuntimeError(
                    f"Remote-agent runtime is missing its interpreter: {source}"
                )
    resolved_outputs: list[Path] = []
    for path, kind in outputs:
        path = (source_root / path).resolve()
        if kind in {"archive", "cached"} and archive_format_for_path(path) == "tar.zst":
            resolve_zstd_command()
        for previous in resolved_outputs:
            if path == previous or path in previous.parents or previous in path.parents:
                raise RuntimeError(f"Release outputs overlap: {previous} and {path}")
        resolved_outputs.append(path)
        if path.exists():
            directory = kind in {"package", "directory"}
            if path.is_dir() != directory:
                raise RuntimeError(f"Release output has the wrong file type: {path}")
            empty_package = kind == "package" and not any(path.iterdir())
            if (
                not args.force
                and not getattr(args, "resume", False)
                and kind != "cached"
                and not empty_package
            ):
                raise RuntimeError(f"Release output already exists: {path}")
        parent = path.parent
        while not parent.exists():
            parent = parent.parent
        if not parent.is_dir() or not os.access(parent, os.W_OK | os.X_OK):
            raise RuntimeError(f"Release output parent is not writable: {parent}")
    notarize = (include_macos and (not args.skip_github_release or args.notarize)) or (
        macos_cli and args.notarize and not args.skip_github_release
    )
    if notarize:
        command = [
            str(signing_dir / "notarize_macos_binary_with_rcodesign.sh"),
            "--preflight",
            "--report-dir",
            str(output_dir / version / "notarization/preflight"),
        ]
        if args.notarytool_keychain_profile:
            command.extend(
                ["--notarytool-keychain-profile", args.notarytool_keychain_profile]
            )
        print("+ " + shlex.join(command), flush=True)
        try:
            subprocess.run(command, cwd=source_root, check=True)
        except (OSError, subprocess.CalledProcessError) as error:
            raise RuntimeError("macOS notarization preflight failed.") from error
        resolved_profile = (
            output_dir / version / "notarization/preflight/notarytool-profile.txt"
        )
        if resolved_profile.is_file():
            profile = resolved_profile.read_text(encoding="utf-8").strip()
            if profile:
                args.notarytool_keychain_profile = profile
    try:
        if include_ios or include_macos:
            subprocess.run(["xcodebuild", "-version"], check=True)
        if include_ios:
            subprocess.run(
                ["xcrun", "--sdk", "iphoneos", "--show-sdk-path"], check=True
            )
        if include_macos or macos_cli:
            with tempfile.TemporaryDirectory(prefix="xedoc-signing-preflight-") as temp:
                probe = Path(temp) / "signing-probe"
                shutil.copyfile("/usr/bin/true", probe)
                probe.chmod(0o755)
                subprocess.run(
                    [
                        str(signing_dir / "sign_macos_code.sh"),
                        "--target",
                        str(probe),
                        "--identity",
                        codesign_identity,
                        "--options",
                        "runtime",
                        "--timestamp",
                        "true",
                        "--entitlements",
                        str(signing_dir / "xedoc.entitlements.plist"),
                    ],
                    check=True,
                )
                subprocess.run(
                    ["codesign", "--verify", "--strict", str(probe)], check=True
                )
    except (OSError, subprocess.CalledProcessError) as error:
        raise RuntimeError(
            f"Apple toolchain or signing preflight failed: {error}"
        ) from error
