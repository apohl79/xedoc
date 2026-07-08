#!/usr/bin/env python3

import argparse
import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import apohl79_release
from apohl79_release import build_codesign_command
from apohl79_release import derive_fork_version
from apohl79_release import latest_release_version_from_ls_remote
from apohl79_release import main
from apohl79_release import run


class Apohl79ReleaseTest(unittest.TestCase):
    def test_release_version_uses_cargo_version_when_not_sentinel(self) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.141.0",
                ls_remote_stdout=(
                    "abc\trefs/tags/rust-v0.140.0-alpha.19\n"
                    "def\trefs/tags/rust-v0.140.0-alpha.19^{}\n"
                ),
                build_number=1,
            ),
            "0.141.0-apohl79-1",
        )

    def test_release_version_falls_back_to_latest_valid_upstream_tag(self) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.0.0",
                describe_tag=None,
                ls_remote_stdout=(
                    "aaa\trefs/tags/rust-v0.140.0-alpha.9\n"
                    "bbb\trefs/tags/rust-vrust-v0.999.0\n"
                    "ccc\trefs/tags/rust-v0.140.0-alpha.10^{}\n"
                    "ddd\trefs/tags/rust-v0.139.0\n"
                    "eee\trefs/tags/rust-v0.140.0-alpha.10\n"
                ),
                build_number=12,
            ),
            "0.140.0-alpha.10-apohl79-12",
        )

    def test_release_version_uses_latest_upstream_tag_instead_of_reachable_tag(
        self,
    ) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.0.0",
                describe_tag="rust-v0.139.0",
                ls_remote_stdout="aaa\trefs/tags/rust-v0.140.0-alpha.10\n",
                build_number=7,
            ),
            "0.140.0-alpha.10-apohl79-7",
        )

    def test_release_version_ignores_reachable_fork_tag(self) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.0.0",
                describe_tag="rust-v0.140.0-alpha.10-apohl79-41",
                ls_remote_stdout=(
                    "aaa\trefs/tags/rust-v0.140.0-alpha.20\n"
                    "bbb\trefs/tags/rust-v0.140.0-alpha.21\n"
                ),
                build_number=42,
            ),
            "0.140.0-alpha.21-apohl79-42",
        )

    def test_read_fork_build_number_accepts_minimum(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "apohl79_build_number.txt"
            path.write_text("1\n", encoding="utf-8")

            self.assertEqual(apohl79_release.read_fork_build_number(path), 1)

    def test_read_fork_build_number_rejects_zero(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "apohl79_build_number.txt"
            path.write_text("0\n", encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError, "must be at least 1"):
                apohl79_release.read_fork_build_number(path)

    def test_read_fork_build_number_rejects_non_integer(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "apohl79_build_number.txt"
            path.write_text("build-1\n", encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError, "must be a positive integer"):
                apohl79_release.read_fork_build_number(path)

    def test_latest_release_version_rejects_missing_valid_tags(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "No valid upstream rust release tags"
        ):
            latest_release_version_from_ls_remote("aaa\trefs/tags/rust-vrust-v0.1.0\n")

    def test_github_release_tag_uses_rust_prefix(self) -> None:
        self.assertEqual(
            apohl79_release.github_release_tag("0.141.0-apohl79-1"),
            "rust-v0.141.0-apohl79-1",
        )

    def test_github_release_env_uses_configured_account_token(self) -> None:
        with (
            mock.patch.dict(os.environ, {}, clear=True),
            mock.patch.object(
                subprocess,
                "check_output",
                return_value="secret-token\n",
            ) as check_output,
        ):
            env = apohl79_release.github_release_env(
                gh="gh",
                account="apohl79",
            )

        self.assertIsNotNone(env)
        assert env is not None
        self.assertEqual(env["GH_TOKEN"], "secret-token")
        check_output.assert_called_once_with(
            ["gh", "auth", "token", "-h", "github.com", "-u", "apohl79"],
            text=True,
            stderr=subprocess.DEVNULL,
        )

    def test_github_release_env_respects_existing_token(self) -> None:
        with (
            mock.patch.dict(os.environ, {"GH_TOKEN": "existing-token"}),
            mock.patch.object(
                subprocess,
                "check_output",
                side_effect=AssertionError("should not read gh auth token"),
            ),
        ):
            self.assertIsNone(
                apohl79_release.github_release_env(
                    gh="gh",
                    account="apohl79",
                )
            )

    def test_publish_github_release_creates_missing_release_and_uploads_archives(
        self,
    ) -> None:
        commands = []

        def fake_run(
            command: list[str],
            *,
            cwd: Path | None = None,
            env: dict[str, str] | None = None,
            check: bool = True,
            stdout: int | None = None,
        ) -> subprocess.CompletedProcess:
            _ = env
            commands.append((command, cwd, check, stdout))
            returncode = 1 if command[:3] == ["gh", "release", "view"] else 0
            return subprocess.CompletedProcess(command, returncode)

        with mock.patch.object(apohl79_release, "run", side_effect=fake_run):
            apohl79_release.publish_github_release(
                gh="gh",
                repo="apohl79/codex",
                tag="rust-v0.141.0-apohl79-1",
                title="0.141.0-apohl79-1",
                target="abc123",
                archive_outputs=[Path("/tmp/codex.zip")],
            )

        self.assertEqual(
            commands,
            [
                (
                    [
                        "gh",
                        "release",
                        "view",
                        "rust-v0.141.0-apohl79-1",
                        "--repo",
                        "apohl79/codex",
                    ],
                    apohl79_release.REPO_ROOT,
                    False,
                    subprocess.DEVNULL,
                ),
                (
                    [
                        "gh",
                        "release",
                        "create",
                        "rust-v0.141.0-apohl79-1",
                        "--repo",
                        "apohl79/codex",
                        "--title",
                        "0.141.0-apohl79-1",
                        "--notes",
                        "apohl79 Codex 0.141.0-apohl79-1",
                        "--target",
                        "abc123",
                    ],
                    apohl79_release.REPO_ROOT,
                    True,
                    None,
                ),
                (
                    [
                        "gh",
                        "release",
                        "upload",
                        "rust-v0.141.0-apohl79-1",
                        "/tmp/codex.zip",
                        "--repo",
                        "apohl79/codex",
                        "--clobber",
                    ],
                    apohl79_release.REPO_ROOT,
                    True,
                    None,
                ),
            ],
        )

    def test_publish_github_release_reuses_existing_release(self) -> None:
        commands = []

        def fake_run(
            command: list[str],
            *,
            cwd: Path | None = None,
            env: dict[str, str] | None = None,
            check: bool = True,
            stdout: int | None = None,
        ) -> subprocess.CompletedProcess:
            _ = env
            commands.append((command, cwd, check, stdout))
            return subprocess.CompletedProcess(command, 0)

        with mock.patch.object(apohl79_release, "run", side_effect=fake_run):
            apohl79_release.publish_github_release(
                gh="gh",
                repo="apohl79/codex",
                tag="rust-v0.141.0-apohl79-1",
                title="0.141.0-apohl79-1",
                target="abc123",
                archive_outputs=[Path("/tmp/codex-a.zip"), Path("/tmp/codex-b.zip")],
            )

        command_names = [command[:3] for command, *_ in commands]
        self.assertNotIn(["gh", "release", "create"], command_names)
        self.assertEqual(
            command_names,
            [
                ["gh", "release", "view"],
                ["gh", "release", "upload"],
                ["gh", "release", "upload"],
            ],
        )

    def test_codesign_command_matches_release_binary_signing_shape(self) -> None:
        self.assertEqual(
            build_codesign_command(
                target=Path("/tmp/codex"),
                identity="Developer ID Application: Example",
                entitlements=Path(
                    ".github/scripts/macos-signing/codex.entitlements.plist"
                ),
            ),
            [
                ".github/scripts/macos-signing/sign_macos_code.sh",
                "--target",
                "/tmp/codex",
                "--identity",
                "Developer ID Application: Example",
                "--deep",
                "false",
                "--identifier",
                "codex",
                "--options",
                "runtime",
                "--timestamp",
                "true",
                "--entitlements",
                ".github/scripts/macos-signing/codex.entitlements.plist",
            ],
        )

    def test_build_release_uses_current_checkout_without_patching_manifests(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            cargo_toml = repo_root / "codex-rs" / "Cargo.toml"
            cargo_lock = repo_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            original_cargo_toml = "\n".join(
                [
                    "[workspace]",
                    'members = ["cli"]',
                    "",
                    "[workspace.package]",
                    'version = "0.141.0"',
                    'edition = "2024"',
                    "",
                ]
            )
            original_cargo_lock = "version = 4\n"
            cargo_toml.write_text(original_cargo_toml, encoding="utf-8")
            cargo_lock.write_text(original_cargo_lock, encoding="utf-8")
            (repo_root / "scripts").mkdir()
            (repo_root / "scripts/apohl79_build_number.txt").write_text(
                "9\n",
                encoding="utf-8",
            )
            (repo_root / ".github/scripts/macos-signing").mkdir(parents=True)
            target_dir = repo_root / "target"
            commands = []
            manifest_snapshots = {}
            lock_snapshots = {}

            def fake_run(
                command: list[str],
                *,
                cwd: Path | None = None,
                env: dict[str, str] | None = None,
                check: bool = True,
            ) -> subprocess.CompletedProcess:
                commands.append((command, cwd, env, check))
                if command[:2] == ["cargo", "build"]:
                    manifest_snapshots["cargo_build"] = cargo_toml.read_text(
                        encoding="utf-8"
                    )
                    lock_snapshots["cargo_build"] = cargo_lock.read_text(
                        encoding="utf-8"
                    )
                    entrypoint = (
                        target_dir / "aarch64-apple-darwin" / "release" / "codex"
                    )
                    entrypoint.parent.mkdir(parents=True)
                    entrypoint.write_text("codex", encoding="utf-8")
                    entrypoint.chmod(0o755)
                if command[:1] == [sys.executable]:
                    manifest_snapshots["package"] = cargo_toml.read_text(
                        encoding="utf-8"
                    )
                    lock_snapshots["package"] = cargo_lock.read_text(encoding="utf-8")
                return subprocess.CompletedProcess(command, 0)

            args = argparse.Namespace(
                archive_output=[],
                cargo="cargo",
                codesign_identity=None,
                force=True,
                gh="gh",
                github_account="apohl79",
                github_repo="apohl79/codex",
                keep_worktree=False,
                output_dir=Path("dist/apohl79"),
                package_dir=None,
                ref="main-fork",
                skip_github_release=True,
                target="aarch64-apple-darwin",
                version_suffix="apohl79",
            )

            with (
                mock.patch.object(apohl79_release, "REPO_ROOT", repo_root),
                mock.patch.object(
                    apohl79_release,
                    "ensure_current_checkout_matches_ref",
                    return_value=None,
                ),
                mock.patch.object(
                    apohl79_release, "ensure_git_path_clean", return_value=None
                ),
                mock.patch.object(
                    apohl79_release,
                    "resolve_codesign_identity",
                    return_value="Developer ID Application: Example",
                ),
                mock.patch.object(
                    apohl79_release,
                    "default_cargo_build_jobs",
                    return_value=4,
                ),
                mock.patch.object(apohl79_release, "run", side_effect=fake_run),
                mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target_dir)}),
            ):
                apohl79_release.build_release(args)

            self.assertEqual(
                cargo_toml.read_text(encoding="utf-8"),
                original_cargo_toml,
            )
            self.assertEqual(
                cargo_lock.read_text(encoding="utf-8"),
                original_cargo_lock,
            )
            self.assertEqual(
                manifest_snapshots,
                {"cargo_build": original_cargo_toml, "package": original_cargo_toml},
            )
            self.assertEqual(
                lock_snapshots,
                {"cargo_build": original_cargo_lock, "package": original_cargo_lock},
            )
            self.assertFalse(
                any(
                    command[:3] == ["git", "worktree", "add"]
                    for command, *_ in commands
                )
            )
            cargo_builds = [
                (command, cwd, env)
                for command, cwd, env, _check in commands
                if command[:2] == ["cargo", "build"]
            ]
            self.assertEqual(len(cargo_builds), 1)
            cargo_command, cargo_cwd, cargo_env = cargo_builds[0]
            self.assertEqual(cargo_cwd, repo_root / "codex-rs")
            self.assertIn("--locked", cargo_command)
            self.assertIn(str(cargo_toml), cargo_command)
            self.assertIsNotNone(cargo_env)
            assert cargo_env is not None
            self.assertEqual(cargo_env["CARGO_TARGET_DIR"], str(target_dir.resolve()))
            self.assertEqual(cargo_env["CODEX_RELEASE_VERSION"], "0.141.0-apohl79-9")
            self.assertEqual(cargo_env["CARGO_BUILD_JOBS"], "4")
            package_commands = [
                command
                for command, _cwd, _env, _check in commands
                if command[:1] == [sys.executable]
            ]
            self.assertEqual(len(package_commands), 1)
            package_command = package_commands[0]
            version_arg_index = package_command.index("--version")
            self.assertEqual(
                package_command[version_arg_index + 1],
                "0.141.0-apohl79-9",
            )
            signing_commands = [
                command
                for command, _cwd, _env, _check in commands
                if command and command[0].endswith("sign_macos_code.sh")
            ]
            self.assertEqual(len(signing_commands), 1)
            self.assertIn("Developer ID Application: Example", signing_commands[0])

    def test_build_release_publishes_github_release_after_packaging(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            cargo_toml = repo_root / "codex-rs" / "Cargo.toml"
            cargo_lock = repo_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            cargo_toml.write_text(
                "\n".join(
                    [
                        "[workspace.package]",
                        'version = "0.141.0"',
                        'edition = "2024"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            cargo_lock.write_text("version = 4\n", encoding="utf-8")
            (repo_root / "scripts").mkdir()
            (repo_root / "scripts/apohl79_build_number.txt").write_text(
                "10\n",
                encoding="utf-8",
            )
            (repo_root / ".github/scripts/macos-signing").mkdir(parents=True)
            target_dir = repo_root / "target"
            archive_output = Path("dist/apohl79/custom.zip")
            published = {}

            def fake_run(
                command: list[str],
                *,
                cwd: Path | None = None,
                env: dict[str, str] | None = None,
                check: bool = True,
                stdout: int | None = None,
            ) -> subprocess.CompletedProcess:
                _ = cwd
                _ = env
                _ = check
                _ = stdout
                if command[:2] == ["cargo", "build"]:
                    entrypoint = (
                        target_dir / "aarch64-apple-darwin" / "release" / "codex"
                    )
                    entrypoint.parent.mkdir(parents=True)
                    entrypoint.write_text("codex", encoding="utf-8")
                    entrypoint.chmod(0o755)
                return subprocess.CompletedProcess(command, 0)

            def fake_publish_github_release(
                *,
                gh: str,
                repo: str,
                tag: str,
                title: str,
                target: str,
                archive_outputs: list[Path],
                env: dict[str, str] | None = None,
            ) -> None:
                published.update(
                    {
                        "gh": gh,
                        "repo": repo,
                        "tag": tag,
                        "title": title,
                        "target": target,
                        "archive_outputs": archive_outputs,
                        "env": env,
                    }
                )

            args = argparse.Namespace(
                archive_output=[archive_output],
                cargo="cargo",
                codesign_identity=None,
                force=True,
                gh="custom-gh",
                github_account="apohl79",
                github_repo="apohl79/codex",
                keep_worktree=False,
                output_dir=Path("dist/apohl79"),
                package_dir=None,
                ref="main-fork",
                skip_github_release=False,
                target="aarch64-apple-darwin",
                version_suffix="apohl79",
            )

            with (
                mock.patch.object(apohl79_release, "REPO_ROOT", repo_root),
                mock.patch.object(
                    apohl79_release,
                    "ensure_current_checkout_matches_ref",
                    return_value=None,
                ),
                mock.patch.object(
                    apohl79_release, "ensure_git_path_clean", return_value=None
                ),
                mock.patch.object(
                    apohl79_release,
                    "resolve_codesign_identity",
                    return_value="Developer ID Application: Example",
                ),
                mock.patch.object(apohl79_release, "git_commit", return_value="c" * 40),
                mock.patch.object(
                    apohl79_release,
                    "github_release_env",
                    return_value={"GH_TOKEN": "secret-token"},
                ),
                mock.patch.object(apohl79_release, "run", side_effect=fake_run),
                mock.patch.object(
                    apohl79_release,
                    "publish_github_release",
                    side_effect=fake_publish_github_release,
                ),
                mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target_dir)}),
            ):
                apohl79_release.build_release(args)

            self.assertEqual(
                published,
                {
                    "gh": "custom-gh",
                    "repo": "apohl79/codex",
                    "tag": "rust-v0.141.0-apohl79-10",
                    "title": "0.141.0-apohl79-10",
                    "target": "c" * 40,
                    "archive_outputs": [(repo_root / archive_output).resolve()],
                    "env": {"GH_TOKEN": "secret-token"},
                },
            )

    def test_build_release_repairs_stale_cargo_lock_before_locked_build(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            cargo_toml = repo_root / "codex-rs" / "Cargo.toml"
            cargo_lock = repo_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            (repo_root / "MODULE.bazel").write_text("", encoding="utf-8")
            original_cargo_toml = "\n".join(
                [
                    "[workspace]",
                    'members = ["cli"]',
                    "",
                    "[workspace.package]",
                    'version = "0.141.0-alpha.5"',
                    'edition = "2024"',
                    "",
                ]
            )
            stale_cargo_lock = "\n".join(
                [
                    "version = 4",
                    "",
                    "[[package]]",
                    'name = "codex-cli"',
                    'version = "0.0.0"',
                    "",
                    "[[package]]",
                    'name = "codex-core"',
                    'version = "0.0.0"',
                    "",
                    "[[package]]",
                    'name = "external"',
                    'version = "1.2.3"',
                    'source = "registry+https://github.com/rust-lang/crates.io-index"',
                    "",
                ]
            )
            repaired_cargo_lock = stale_cargo_lock.replace(
                'version = "0.0.0"', 'version = "0.141.0-alpha.5"'
            )
            cargo_toml.write_text(original_cargo_toml, encoding="utf-8")
            cargo_lock.write_text(stale_cargo_lock, encoding="utf-8")
            (repo_root / "scripts").mkdir()
            (repo_root / "scripts/apohl79_build_number.txt").write_text(
                "11\n",
                encoding="utf-8",
            )
            (repo_root / ".github/scripts/macos-signing").mkdir(parents=True)
            target_dir = repo_root / "target"
            commands = []
            lock_snapshots = {}

            def fake_run(
                command: list[str],
                *,
                cwd: Path | None = None,
                env: dict[str, str] | None = None,
                check: bool = True,
                stdout: int | None = None,
            ) -> subprocess.CompletedProcess:
                commands.append((command, cwd, env, check, stdout))
                if command[:2] == ["cargo", "metadata"]:
                    self.assertEqual(cwd, repo_root / "codex-rs")
                    self.assertIn("--filter-platform", command)
                    self.assertEqual(stdout, subprocess.DEVNULL)
                    cargo_lock.write_text(repaired_cargo_lock, encoding="utf-8")
                if command[:2] == ["cargo", "build"]:
                    lock_snapshots["cargo_build"] = cargo_lock.read_text(
                        encoding="utf-8"
                    )
                    entrypoint = (
                        target_dir / "aarch64-apple-darwin" / "release" / "codex"
                    )
                    entrypoint.parent.mkdir(parents=True)
                    entrypoint.write_text("codex", encoding="utf-8")
                    entrypoint.chmod(0o755)
                if command[:1] == [sys.executable]:
                    lock_snapshots["package"] = cargo_lock.read_text(encoding="utf-8")
                return subprocess.CompletedProcess(command, 0)

            args = argparse.Namespace(
                archive_output=[],
                cargo="cargo",
                codesign_identity=None,
                force=True,
                gh="gh",
                github_account="apohl79",
                github_repo="apohl79/codex",
                keep_worktree=False,
                output_dir=Path("dist/apohl79"),
                package_dir=None,
                ref="main-fork",
                skip_github_release=True,
                target="aarch64-apple-darwin",
                version_suffix="apohl79",
            )

            with (
                mock.patch.object(apohl79_release, "REPO_ROOT", repo_root),
                mock.patch.object(
                    apohl79_release,
                    "ensure_current_checkout_matches_ref",
                    return_value=None,
                ),
                mock.patch.object(
                    apohl79_release, "ensure_git_path_clean", return_value=None
                ),
                mock.patch.object(
                    apohl79_release,
                    "resolve_codesign_identity",
                    return_value="Developer ID Application: Example",
                ),
                mock.patch.object(
                    apohl79_release,
                    "default_cargo_build_jobs",
                    return_value=4,
                ),
                mock.patch.object(apohl79_release, "run", side_effect=fake_run),
                mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target_dir)}),
            ):
                apohl79_release.build_release(args)

            self.assertEqual(
                cargo_lock.read_text(encoding="utf-8"), repaired_cargo_lock
            )
            self.assertEqual(
                lock_snapshots,
                {"cargo_build": repaired_cargo_lock, "package": repaired_cargo_lock},
            )
            command_names = [command[:2] for command, *_ in commands]
            self.assertLess(
                command_names.index(["cargo", "metadata"]),
                command_names.index(["cargo", "build"]),
            )
            self.assertLess(
                command_names.index(["just", "bazel-lock-update"]),
                command_names.index(["cargo", "build"]),
            )
            self.assertLess(
                command_names.index(["just", "bazel-lock-check"]),
                command_names.index(["cargo", "build"]),
            )

    def test_stale_workspace_lock_packages_ignores_registry_packages(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            cargo_lock = Path(temp_dir) / "Cargo.lock"
            cargo_lock.write_text(
                "\n".join(
                    [
                        "version = 4",
                        "",
                        "[[package]]",
                        'name = "codex-cli"',
                        'version = "0.0.0"',
                        "",
                        "[[package]]",
                        'name = "external"',
                        'version = "1.2.3"',
                        'source = "registry+https://github.com/rust-lang/crates.io-index"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            self.assertEqual(
                apohl79_release.stale_workspace_lock_packages(
                    cargo_lock, "0.141.0-alpha.5"
                ),
                ["codex-cli=0.0.0"],
            )

    def test_repair_stale_release_lockfiles_raises_when_still_stale(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            source_root = Path(temp_dir)
            cargo_toml = source_root / "codex-rs" / "Cargo.toml"
            cargo_lock = source_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            cargo_toml.write_text(
                '[workspace.package]\nversion = "0.141.0-alpha.5"\n',
                encoding="utf-8",
            )
            cargo_lock.write_text(
                "\n".join(
                    [
                        "version = 4",
                        "",
                        "[[package]]",
                        'name = "codex-cli"',
                        'version = "0.0.0"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            with (
                mock.patch.object(apohl79_release, "run", return_value=None),
                self.assertRaises(RuntimeError) as ctx,
            ):
                apohl79_release.repair_stale_release_lockfiles(
                    cargo="cargo",
                    source_root=source_root,
                    cargo_toml=cargo_toml,
                    cargo_lock=cargo_lock,
                    target="aarch64-apple-darwin",
                )

            self.assertIn("codex-cli=0.0.0", str(ctx.exception))

    def test_repair_stale_release_lockfiles_no_op_when_lock_in_sync(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            source_root = Path(temp_dir)
            cargo_toml = source_root / "codex-rs" / "Cargo.toml"
            cargo_lock = source_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            cargo_toml.write_text(
                '[workspace.package]\nversion = "0.141.0-alpha.5"\n',
                encoding="utf-8",
            )
            cargo_lock.write_text(
                "\n".join(
                    [
                        "version = 4",
                        "",
                        "[[package]]",
                        'name = "codex-cli"',
                        'version = "0.141.0-alpha.5"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            with mock.patch.object(apohl79_release, "run") as run_mock:
                apohl79_release.repair_stale_release_lockfiles(
                    cargo="cargo",
                    source_root=source_root,
                    cargo_toml=cargo_toml,
                    cargo_lock=cargo_lock,
                    target="aarch64-apple-darwin",
                )

            run_mock.assert_not_called()

    def test_repair_stale_release_lockfiles_skips_sentinel_version(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            source_root = Path(temp_dir)
            cargo_toml = source_root / "codex-rs" / "Cargo.toml"
            cargo_lock = source_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            cargo_toml.write_text(
                "[workspace.package]\n"
                f'version = "{apohl79_release.WORKSPACE_VERSION_SENTINEL}"\n',
                encoding="utf-8",
            )
            cargo_lock.write_text("version = 4\n", encoding="utf-8")

            with mock.patch.object(apohl79_release, "run") as run_mock:
                apohl79_release.repair_stale_release_lockfiles(
                    cargo="cargo",
                    source_root=source_root,
                    cargo_toml=cargo_toml,
                    cargo_lock=cargo_lock,
                    target="aarch64-apple-darwin",
                )

            run_mock.assert_not_called()

    def test_build_release_leaves_manifests_unchanged_after_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo_root = Path(temp_dir)
            cargo_toml = repo_root / "codex-rs" / "Cargo.toml"
            cargo_lock = repo_root / "codex-rs" / "Cargo.lock"
            cargo_toml.parent.mkdir(parents=True)
            original_cargo_toml = "\n".join(
                [
                    "[workspace.package]",
                    'version = "0.141.0"',
                    'edition = "2024"',
                    "",
                ]
            )
            original_cargo_lock = "version = 4\n"
            cargo_toml.write_text(original_cargo_toml, encoding="utf-8")
            cargo_lock.write_text(original_cargo_lock, encoding="utf-8")
            (repo_root / "scripts").mkdir()
            (repo_root / "scripts/apohl79_build_number.txt").write_text(
                "12\n",
                encoding="utf-8",
            )
            target_dir = repo_root / "target"

            def failing_run(
                command: list[str],
                *,
                cwd: Path | None = None,
                env: dict[str, str] | None = None,
                check: bool = True,
            ) -> subprocess.CompletedProcess:
                _ = cwd
                _ = env
                _ = check
                if command[:2] == ["cargo", "build"]:
                    self.assertEqual(
                        cargo_toml.read_text(encoding="utf-8"),
                        original_cargo_toml,
                    )
                    self.assertEqual(
                        cargo_lock.read_text(encoding="utf-8"),
                        original_cargo_lock,
                    )
                    raise RuntimeError("cargo failed")
                return subprocess.CompletedProcess(command, 0)

            args = argparse.Namespace(
                archive_output=[],
                cargo="cargo",
                codesign_identity=None,
                force=True,
                gh="gh",
                github_account="apohl79",
                github_repo="apohl79/codex",
                keep_worktree=False,
                output_dir=Path("dist/apohl79"),
                package_dir=None,
                ref="main-fork",
                skip_github_release=True,
                target="aarch64-apple-darwin",
                version_suffix="apohl79",
            )

            with (
                mock.patch.object(apohl79_release, "REPO_ROOT", repo_root),
                mock.patch.object(
                    apohl79_release,
                    "ensure_current_checkout_matches_ref",
                    return_value=None,
                ),
                mock.patch.object(
                    apohl79_release, "ensure_git_path_clean", return_value=None
                ),
                mock.patch.object(
                    apohl79_release,
                    "resolve_codesign_identity",
                    return_value="Developer ID Application: Example",
                ),
                mock.patch.object(apohl79_release, "run", side_effect=failing_run),
                mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target_dir)}),
            ):
                with self.assertRaisesRegex(RuntimeError, "cargo failed"):
                    apohl79_release.build_release(args)

            self.assertEqual(
                cargo_toml.read_text(encoding="utf-8"),
                original_cargo_toml,
            )
            self.assertEqual(
                cargo_lock.read_text(encoding="utf-8"),
                original_cargo_lock,
            )

    def test_current_checkout_ref_mismatch_reports_commits(self) -> None:
        with mock.patch.object(
            apohl79_release,
            "git_commit",
            side_effect=["a" * 40, "b" * 40],
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "Current checkout HEAD \\(aaaaaaaaaaaa\\) does not match --ref main-fork",
            ):
                apohl79_release.ensure_current_checkout_matches_ref("main-fork")

    def test_main_rejects_placeholder_codesign_identity_before_build(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            exit_code = main(
                [
                    "--codesign-identity",
                    "Developer ID Application: YOUR NAME (TEAMID)",
                ]
            )

        self.assertEqual(exit_code, 1)
        self.assertEqual(
            stderr.getvalue(),
            "Error: APPLE_CODESIGN_IDENTITY still contains the placeholder value. "
            "Set it to a valid Developer ID Application identity or pass "
            "--codesign-identity.\n",
        )

    def test_codesign_identity_resolver_uses_explicit_native_identity(self) -> None:
        with (
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": ""}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                return_value={
                    "ABCDEF0123456789",
                    "Developer ID Application: Example (TEAMID)",
                },
            ),
        ):
            self.assertEqual(
                apohl79_release.resolve_codesign_identity(
                    "Developer ID Application: Example (TEAMID)"
                ),
                "Developer ID Application: Example (TEAMID)",
            )

    def test_codesign_identity_resolver_rejects_missing_explicit_native_identity(
        self,
    ) -> None:
        with (
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": ""}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                return_value={"Developer ID Application: Other (TEAMID)"},
            ),
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "No native codesign identity named 'Developer ID Application: Example",
            ):
                apohl79_release.resolve_codesign_identity(
                    "Developer ID Application: Example (TEAMID)"
                )

    def test_codesign_identity_resolver_auto_selects_single_developer_id(
        self,
    ) -> None:
        with (
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": ""}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                return_value={
                    "ABCDEF0123456789",
                    "Developer ID Application: Example (TEAMID)",
                    "Apple Development: Example (TEAMID)",
                },
            ),
        ):
            self.assertEqual(
                apohl79_release.resolve_codesign_identity(None),
                "Developer ID Application: Example (TEAMID)",
            )

    def test_codesign_identity_resolver_rejects_missing_developer_id(self) -> None:
        with (
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": ""}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                return_value={"Apple Development: Example (TEAMID)"},
            ),
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "No Developer ID Application codesign identity was found",
            ):
                apohl79_release.resolve_codesign_identity(None)

    def test_codesign_identity_resolver_rejects_ambiguous_developer_ids(self) -> None:
        with (
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": ""}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                return_value={
                    "Developer ID Application: One (TEAMID)",
                    "Developer ID Application: Two (TEAMID)",
                },
            ),
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "Multiple Developer ID Application codesign identities were found",
            ):
                apohl79_release.resolve_codesign_identity(None)

    def test_codesign_identity_resolver_skips_keychain_for_akv_backend(self) -> None:
        with (
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": "akv-pkcs11"}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                side_effect=AssertionError("should not read keychain"),
            ),
        ):
            self.assertEqual(
                apohl79_release.resolve_codesign_identity(None),
                "akv-pkcs11",
            )

    def test_native_codesign_identities_parses_hashes_and_names(self) -> None:
        stdout = (
            '  1) ABCDEF0123456789 "Developer ID Application: Example (TEAMID)"\n'
            '  2) 0123456789ABCDEF "Apple Development: Example (TEAMID)"\n'
            "     2 valid identities found\n"
        )

        with mock.patch.object(
            subprocess,
            "check_output",
            return_value=stdout,
        ):
            self.assertEqual(
                apohl79_release.native_codesign_identities(),
                {
                    "ABCDEF0123456789",
                    "Developer ID Application: Example (TEAMID)",
                    "0123456789ABCDEF",
                    "Apple Development: Example (TEAMID)",
                },
            )

    def test_default_cargo_build_jobs_uses_apple_performance_cores(self) -> None:
        def fake_sysctl_int(name: str) -> int | None:
            return {
                "hw.perflevel0.physicalcpu": 4,
                "hw.physicalcpu": 10,
                "hw.logicalcpu": 10,
            }.get(name)

        with mock.patch.object(
            apohl79_release,
            "sysctl_int",
            side_effect=fake_sysctl_int,
        ):
            self.assertEqual(apohl79_release.default_cargo_build_jobs(), 4)

    def test_default_cargo_build_jobs_falls_back_to_physical_cores(self) -> None:
        def fake_sysctl_int(name: str) -> int | None:
            return {
                "hw.perflevel0.physicalcpu": None,
                "hw.physicalcpu": 8,
                "hw.logicalcpu": 16,
            }.get(name)

        with mock.patch.object(
            apohl79_release,
            "sysctl_int",
            side_effect=fake_sysctl_int,
        ):
            self.assertEqual(apohl79_release.default_cargo_build_jobs(), 8)

    def test_default_cargo_build_jobs_falls_back_to_one(self) -> None:
        with (
            mock.patch.object(apohl79_release, "sysctl_int", return_value=None),
            mock.patch.object(apohl79_release.os, "cpu_count", return_value=None),
        ):
            self.assertEqual(apohl79_release.default_cargo_build_jobs(), 1)

    def test_parse_args_accepts_cargo_build_jobs_lower_bound(self) -> None:
        args = apohl79_release.parse_args(["--cargo-build-jobs", "1"])

        self.assertEqual(args.cargo_build_jobs, 1)

    def test_parse_args_rejects_non_positive_cargo_build_jobs(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                apohl79_release.parse_args(["--cargo-build-jobs", "0"])

        self.assertEqual(raised.exception.code, 2)
        self.assertIn("must be a positive integer", stderr.getvalue())

    def test_resolve_cargo_build_jobs_prefers_cli_over_fork_env(self) -> None:
        with mock.patch.dict(
            os.environ,
            {apohl79_release.FORK_CARGO_BUILD_JOBS_ENV_VAR: "3"},
        ):
            self.assertEqual(apohl79_release.resolve_cargo_build_jobs(2), "2")

    def test_resolve_cargo_build_jobs_uses_fork_env(self) -> None:
        with mock.patch.dict(
            os.environ,
            {apohl79_release.FORK_CARGO_BUILD_JOBS_ENV_VAR: "3"},
        ):
            self.assertEqual(apohl79_release.resolve_cargo_build_jobs(None), "3")

    def test_resolve_cargo_build_jobs_rejects_invalid_fork_env(self) -> None:
        with mock.patch.dict(
            os.environ,
            {apohl79_release.FORK_CARGO_BUILD_JOBS_ENV_VAR: "0"},
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                f"{apohl79_release.FORK_CARGO_BUILD_JOBS_ENV_VAR} must be a "
                "positive integer.",
            ):
                apohl79_release.resolve_cargo_build_jobs(None)

    def test_sysctl_int_parses_positive_integer(self) -> None:
        with mock.patch.object(
            subprocess,
            "check_output",
            return_value="4\n",
        ):
            self.assertEqual(
                apohl79_release.sysctl_int("hw.perflevel0.physicalcpu"),
                4,
            )

    def test_sysctl_int_ignores_missing_or_invalid_values(self) -> None:
        with mock.patch.object(
            subprocess,
            "check_output",
            side_effect=subprocess.CalledProcessError(1, ["sysctl"]),
        ):
            self.assertIsNone(apohl79_release.sysctl_int("missing"))

        with mock.patch.object(
            subprocess,
            "check_output",
            return_value="not-an-int\n",
        ):
            self.assertIsNone(apohl79_release.sysctl_int("invalid"))

    def test_run_reports_command_failure_as_runtime_error(self) -> None:
        with mock.patch.object(
            subprocess,
            "run",
            side_effect=subprocess.CalledProcessError(7, ["demo", "arg with space"]),
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "Command failed with exit status 7: demo 'arg with space'",
            ):
                run(["demo", "arg with space"])

    def test_main_reports_missing_auto_detected_codesign_identity(self) -> None:
        stderr = io.StringIO()
        with (
            contextlib.redirect_stderr(stderr),
            mock.patch.dict(os.environ, {"OAI_CODESIGN_BACKEND": ""}),
            mock.patch.object(
                apohl79_release,
                "native_codesign_identities",
                return_value={"Apple Development: Example (TEAMID)"},
            ),
        ):
            exit_code = main(["--codesign-identity", ""])

        self.assertEqual(exit_code, 1)
        self.assertIn(
            "Error: No Developer ID Application codesign identity was found.",
            stderr.getvalue(),
        )

    def test_main_rejects_base_version_argument(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                main(["--base-version", "0.140.0-alpha.10"])

        self.assertEqual(raised.exception.code, 2)
        self.assertIn("unrecognized arguments: --base-version", stderr.getvalue())

    def test_help_omits_base_version_and_mentions_zip_default(self) -> None:
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with self.assertRaises(SystemExit) as raised:
                main(["--help"])

        self.assertEqual(raised.exception.code, 0)
        self.assertNotIn("--base-version", stdout.getvalue())
        self.assertIn(".zip", stdout.getvalue())

    def test_shell_wrapper_does_not_pin_version(self) -> None:
        wrapper = (
            Path(__file__)
            .with_name("build_apohl79_release.sh")
            .read_text(encoding="utf-8")
        )

        self.assertNotIn("--base-version", wrapper)
        self.assertNotIn("0.140.0", wrapper)
        self.assertNotIn("YOUR NAME", wrapper)
        self.assertIn("--target aarch64-apple-darwin", wrapper)
        self.assertIn("--github-repo apohl79/codex", wrapper)
        self.assertIn("--github-account apohl79", wrapper)
        self.assertIn('"$@"', wrapper)


if __name__ == "__main__":
    unittest.main()
