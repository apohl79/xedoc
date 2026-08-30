#!/usr/bin/env python3

import contextlib
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import apohl79_bazel_status
import apohl79_release


class Apohl79BazelReleaseTest(unittest.TestCase):
    def test_build_system_defaults_to_bazel_and_accepts_cargo(self) -> None:
        default_args = apohl79_release.parse_args([])
        cargo_args = apohl79_release.parse_args(["--build-system", "cargo"])

        self.assertEqual(
            (default_args.build_system, cargo_args.build_system),
            ("bazel", "cargo"),
        )

    def test_bazel_local_limits_accept_minimum_and_default_to_none(self) -> None:
        default_args = apohl79_release.parse_args([])
        limited_args = apohl79_release.parse_args(
            ["--bazel-build-jobs", "1", "--bazel-max-heap-mb", "1"]
        )

        self.assertEqual(
            (
                default_args.bazel_build_jobs,
                default_args.bazel_max_heap_mb,
                limited_args.bazel_build_jobs,
                limited_args.bazel_max_heap_mb,
            ),
            (None, None, 1, 1),
        )

    def test_bazel_release_options_map_both_macos_targets(self) -> None:
        self.assertEqual(
            (
                apohl79_release.bazel_release_options("aarch64-apple-darwin"),
                apohl79_release.bazel_release_options("x86_64-apple-darwin"),
            ),
            (
                [
                    "--config=apohl79-release",
                    "--platforms=@llvm//platforms:macos_arm64",
                ],
                [
                    "--config=apohl79-release",
                    "--platforms=@llvm//platforms:macos_amd64",
                ],
            ),
        )

    def test_bazel_release_options_reject_unsupported_target(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError,
            "No Bazel release platform for target x86_64-unknown-linux-gnu",
        ):
            apohl79_release.bazel_release_options("x86_64-unknown-linux-gnu")

    def test_build_bazel_release_binaries_builds_bundle_and_resolves_outputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            source_root = Path(temp_dir)
            execution_root = source_root / "execroot"
            entrypoint = execution_root / "bazel-out/release/codex"
            entrypoint.parent.mkdir(parents=True)
            entrypoint.write_text("codex", encoding="utf-8")

            with (
                mock.patch.object(apohl79_release, "run") as run_mock,
                mock.patch.object(
                    apohl79_release,
                    "command_output",
                    side_effect=[
                        f"{execution_root}\n",
                        "bazel-out/release/codex\n",
                    ],
                ) as command_output,
            ):
                result = apohl79_release.build_bazel_release_binaries(
                    bazel="custom-bazel",
                    bazel_build_jobs=4,
                    bazel_max_heap_mb=2048,
                    source_root=source_root,
                    target="aarch64-apple-darwin",
                    fork_version="0.145.0-apohl79-92",
                )

            self.assertEqual(
                result,
                apohl79_release.ReleaseBinaries(entrypoint=entrypoint.resolve()),
            )
            build_command = run_mock.call_args.args[0]
            build_env = run_mock.call_args.kwargs["env"]
            self.assertEqual(
                build_command,
                [
                    "custom-bazel",
                    "--noexperimental_remote_repo_contents_cache",
                    "--host_jvm_args=-Xmx2048m",
                    "build",
                    "--repo_contents_cache=",
                    "--config=apohl79-release",
                    "--platforms=@llvm//platforms:macos_arm64",
                    "--local_resources=cpu=4",
                    "--",
                    "//codex-rs:apohl79-release-binaries",
                ],
            )
            self.assertEqual(
                build_env["CODEX_RELEASE_VERSION"],
                "0.145.0-apohl79-92",
            )
            self.assertEqual(
                [call.args[0] for call in command_output.call_args_list],
                [
                    [
                        "custom-bazel",
                        "--noexperimental_remote_repo_contents_cache",
                        "--host_jvm_args=-Xmx2048m",
                        "info",
                        "--repo_contents_cache=",
                        "execution_root",
                    ],
                    [
                        "custom-bazel",
                        "--noexperimental_remote_repo_contents_cache",
                        "--host_jvm_args=-Xmx2048m",
                        "cquery",
                        "--repo_contents_cache=",
                        "--config=apohl79-release",
                        "--platforms=@llvm//platforms:macos_arm64",
                        "--output=files",
                        "--",
                        "//codex-rs:apohl79-release-binaries",
                    ],
                ],
            )

    def test_resolve_bazel_release_binaries_requires_exact_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            execution_root = Path(temp_dir)
            entrypoint = execution_root / "bazel-out/release/codex"
            entrypoint.parent.mkdir(parents=True)
            entrypoint.write_text("codex", encoding="utf-8")

            self.assertEqual(
                apohl79_release.resolve_bazel_release_binaries(
                    "bazel-out/release/codex\n",
                    execution_root,
                ),
                apohl79_release.ReleaseBinaries(entrypoint.resolve()),
            )
            for stdout in (
                "",
                "bazel-out/release/codex-app-server\n",
                "bazel-out/release/codex\nbazel-out/release/other\n",
            ):
                with self.subTest(stdout=stdout):
                    with self.assertRaisesRegex(
                        RuntimeError,
                        "Bazel release bundle must contain exactly",
                    ):
                        apohl79_release.resolve_bazel_release_binaries(
                            stdout,
                            execution_root,
                        )

    def test_stage_release_binaries_copies_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "source"
            source.mkdir()
            entrypoint = source / "codex"
            entrypoint.write_text("unsigned-codex", encoding="utf-8")

            result = apohl79_release.stage_release_binaries(
                apohl79_release.ReleaseBinaries(entrypoint),
                root / "staged",
            )

            self.assertEqual(
                (result, result.entrypoint.read_text(encoding="utf-8")),
                (
                    apohl79_release.ReleaseBinaries(
                        (root / "staged/codex").resolve(),
                    ),
                    "unsigned-codex",
                ),
            )

    def test_bazel_status_emits_valid_release_version(self) -> None:
        stdout = io.StringIO()
        with (
            mock.patch.dict(
                os.environ,
                {"CODEX_RELEASE_VERSION": "0.145.0-apohl79-92"},
            ),
            contextlib.redirect_stdout(stdout),
        ):
            result = apohl79_bazel_status.main()

        self.assertEqual(result, 0)
        self.assertEqual(
            stdout.getvalue(),
            "STABLE_CODEX_RELEASE_VERSION 0.145.0-apohl79-92\n",
        )

    def test_bazel_status_rejects_missing_or_malformed_version(self) -> None:
        for release_version in ("", "invalid version"):
            with self.subTest(release_version=release_version):
                stderr = io.StringIO()
                with (
                    mock.patch.dict(
                        os.environ,
                        {"CODEX_RELEASE_VERSION": release_version},
                        clear=True,
                    ),
                    contextlib.redirect_stderr(stderr),
                ):
                    result = apohl79_bazel_status.main()

                self.assertEqual(result, 1)
                self.assertEqual(
                    stderr.getvalue(),
                    "CODEX_RELEASE_VERSION must be set to a valid release version.\n",
                )


if __name__ == "__main__":
    unittest.main()
