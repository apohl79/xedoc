#!/usr/bin/env python3

import contextlib
import io
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from apohl79_release import build_codesign_command
from apohl79_release import derive_fork_version
from apohl79_release import latest_release_version_from_ls_remote
from apohl79_release import main
from apohl79_release import patch_workspace_version


class Apohl79ReleaseTest(unittest.TestCase):
    def test_release_version_uses_cargo_version_when_not_sentinel(self) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.141.0",
                ls_remote_stdout=(
                    "abc\trefs/tags/rust-v0.140.0-alpha.19\n"
                    "def\trefs/tags/rust-v0.140.0-alpha.19^{}\n"
                ),
            ),
            "0.141.0-apohl79",
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
            ),
            "0.140.0-alpha.10-apohl79",
        )

    def test_release_version_uses_latest_upstream_tag_instead_of_reachable_tag(self) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.0.0",
                describe_tag="rust-v0.139.0",
                ls_remote_stdout="aaa\trefs/tags/rust-v0.140.0-alpha.10\n",
            ),
            "0.140.0-alpha.10-apohl79",
        )

    def test_release_version_ignores_reachable_fork_tag(self) -> None:
        self.assertEqual(
            derive_fork_version(
                "0.0.0",
                describe_tag="rust-v0.140.0-alpha.10-apohl79",
                ls_remote_stdout=(
                    "aaa\trefs/tags/rust-v0.140.0-alpha.20\n"
                    "bbb\trefs/tags/rust-v0.140.0-alpha.21\n"
                ),
            ),
            "0.140.0-alpha.21-apohl79",
        )

    def test_latest_release_version_rejects_missing_valid_tags(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "No valid upstream rust release tags"
        ):
            latest_release_version_from_ls_remote("aaa\trefs/tags/rust-vrust-v0.1.0\n")

    def test_patch_workspace_version_updates_only_workspace_package(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            cargo_toml = Path(temp_dir) / "Cargo.toml"
            cargo_toml.write_text(
                "\n".join(
                    [
                        "[package]",
                        'version = "9.9.9"',
                        "",
                        "[workspace.package]",
                        'version = "0.0.0"',
                        'edition = "2024"',
                        "",
                        "[workspace.dependencies]",
                        'demo = "1.2.3"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )

            patch_workspace_version(cargo_toml, "0.140.0-alpha.10-apohl79")

            self.assertEqual(
                cargo_toml.read_text(encoding="utf-8"),
                "\n".join(
                    [
                        "[package]",
                        'version = "9.9.9"',
                        "",
                        "[workspace.package]",
                        'version = "0.140.0-alpha.10-apohl79"',
                        'edition = "2024"',
                        "",
                        "[workspace.dependencies]",
                        'demo = "1.2.3"',
                        "",
                    ]
                ),
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

    def test_main_reports_missing_codesign_identity_without_traceback(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            exit_code = main(["--codesign-identity", ""])

        self.assertEqual(exit_code, 1)
        self.assertEqual(
            stderr.getvalue(),
            "Error: Must pass --codesign-identity or set APPLE_CODESIGN_IDENTITY.\n",
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
        wrapper = Path(__file__).with_name("build_apohl79_release.sh").read_text(
            encoding="utf-8"
        )

        self.assertNotIn("--base-version", wrapper)
        self.assertNotIn("0.140.0", wrapper)
        self.assertIn("--target aarch64-apple-darwin", wrapper)


if __name__ == "__main__":
    unittest.main()
