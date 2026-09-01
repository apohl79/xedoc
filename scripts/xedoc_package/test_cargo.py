#!/usr/bin/env python3

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from xedoc_package.cargo import source_binaries_for_target
from xedoc_package.targets import PACKAGE_VARIANTS
from xedoc_package.targets import TARGET_SPECS


class SourceBinariesForTargetTest(unittest.TestCase):
    def test_macos_package_with_prebuilt_entrypoint_builds_nothing(self) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["aarch64-apple-darwin"],
                PACKAGE_VARIANTS["xedoc"],
                build_entrypoint=False,
                build_bwrap=False,
            ),
            [],
        )

    def test_linux_package_with_prebuilt_entrypoint_and_bwrap_builds_nothing(
        self,
    ) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["x86_64-unknown-linux-musl"],
                PACKAGE_VARIANTS["xedoc"],
                build_entrypoint=False,
                build_bwrap=False,
            ),
            [],
        )

    def test_missing_entrypoint_is_built_for_app_server(self) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["aarch64-apple-darwin"],
                PACKAGE_VARIANTS["xedoc-app-server"],
                build_entrypoint=True,
                build_bwrap=False,
            ),
            ["xedoc-app-server"],
        )


if __name__ == "__main__":
    unittest.main()
