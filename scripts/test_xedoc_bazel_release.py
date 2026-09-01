#!/usr/bin/env python3

from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import xedoc_release


class XedocBazelReleaseTest(unittest.TestCase):
    def test_build_system_defaults_to_bazel_and_accepts_cargo(self) -> None:
        default_args = xedoc_release.parse_args([])
        cargo_args = xedoc_release.parse_args(["--build-system", "cargo"])

        self.assertEqual(
            (default_args.build_system, cargo_args.build_system),
            ("bazel", "cargo"),
        )

    def test_bazel_local_limits_accept_minimum_and_default_to_none(self) -> None:
        default_args = xedoc_release.parse_args([])
        limited_args = xedoc_release.parse_args(
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
                xedoc_release.bazel_release_options("aarch64-apple-darwin"),
                xedoc_release.bazel_release_options("x86_64-apple-darwin"),
            ),
            (
                [
                    "--config=xedoc-release",
                    "--platforms=@llvm//platforms:macos_arm64",
                ],
                [
                    "--config=xedoc-release",
                    "--platforms=@llvm//platforms:macos_amd64",
                ],
            ),
        )

    def test_bazel_release_options_reject_unsupported_target(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError,
            "No Bazel release platform for target x86_64-unknown-linux-gnu",
        ):
            xedoc_release.bazel_release_options("x86_64-unknown-linux-gnu")

    def test_build_bazel_release_binaries_builds_bundle_and_resolves_outputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            source_root = Path(temp_dir)
            execution_root = source_root / "execroot"
            entrypoint = execution_root / "bazel-out/release/xedoc"
            entrypoint.parent.mkdir(parents=True)
            entrypoint.write_text("xedoc", encoding="utf-8")

            with (
                mock.patch.object(xedoc_release, "run") as run_mock,
                mock.patch.object(
                    xedoc_release,
                    "command_output",
                    side_effect=[
                        f"{execution_root}\n",
                        "bazel-out/release/xedoc\n",
                    ],
                ) as command_output,
            ):
                result = xedoc_release.build_bazel_release_binaries(
                    bazel="custom-bazel",
                    bazel_build_jobs=4,
                    bazel_max_heap_mb=2048,
                    source_root=source_root,
                    target="aarch64-apple-darwin",
                )

            self.assertEqual(
                result,
                xedoc_release.ReleaseBinaries(entrypoint=entrypoint.resolve()),
            )
            build_command = run_mock.call_args.args[0]
            self.assertEqual(
                build_command,
                [
                    "custom-bazel",
                    "--noexperimental_remote_repo_contents_cache",
                    "--host_jvm_args=-Xmx2048m",
                    "build",
                    "--repo_contents_cache=",
                    "--config=xedoc-release",
                    "--platforms=@llvm//platforms:macos_arm64",
                    "--local_resources=cpu=4",
                    "--",
                    "//xedoc-rs:xedoc-release-binaries",
                ],
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
                        "--config=xedoc-release",
                        "--platforms=@llvm//platforms:macos_arm64",
                        "--output=files",
                        "--",
                        "//xedoc-rs:xedoc-release-binaries",
                    ],
                ],
            )

    def test_resolve_bazel_release_binaries_requires_exact_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            execution_root = Path(temp_dir)
            entrypoint = execution_root / "bazel-out/release/xedoc"
            entrypoint.parent.mkdir(parents=True)
            entrypoint.write_text("xedoc", encoding="utf-8")

            self.assertEqual(
                xedoc_release.resolve_bazel_release_binaries(
                    "bazel-out/release/xedoc\n",
                    execution_root,
                ),
                xedoc_release.ReleaseBinaries(entrypoint.resolve()),
            )
            for stdout in (
                "",
                "bazel-out/release/xedoc-app-server\n",
                "bazel-out/release/xedoc\nbazel-out/release/other\n",
            ):
                with self.subTest(stdout=stdout):
                    with self.assertRaisesRegex(
                        RuntimeError,
                        "Bazel release bundle must contain exactly",
                    ):
                        xedoc_release.resolve_bazel_release_binaries(
                            stdout,
                            execution_root,
                        )

    def test_stage_release_binaries_copies_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "source"
            source.mkdir()
            entrypoint = source / "xedoc"
            entrypoint.write_text("unsigned-xedoc", encoding="utf-8")

            result = xedoc_release.stage_release_binaries(
                xedoc_release.ReleaseBinaries(entrypoint),
                root / "staged",
            )

            self.assertEqual(
                (result, result.entrypoint.read_text(encoding="utf-8")),
                (
                    xedoc_release.ReleaseBinaries(
                        (root / "staged/xedoc").resolve(),
                    ),
                    "unsigned-xedoc",
                ),
            )

    def test_stage_release_binaries_overwrites_read_only_staged_binary(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "source"
            source.mkdir()
            entrypoint = source / "xedoc"
            entrypoint.write_text("rebuilt-xedoc", encoding="utf-8")
            entrypoint.chmod(0o555)
            staged = root / "staged"
            staged.mkdir()
            stale = staged / "xedoc"
            stale.write_text("stale-xedoc", encoding="utf-8")
            stale.chmod(0o555)

            result = xedoc_release.stage_release_binaries(
                xedoc_release.ReleaseBinaries(entrypoint),
                staged,
            )

            self.assertEqual(
                result.entrypoint.read_text(encoding="utf-8"), "rebuilt-xedoc"
            )


if __name__ == "__main__":
    unittest.main()
