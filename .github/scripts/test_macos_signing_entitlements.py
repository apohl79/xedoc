import plistlib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SIGNING_DIR = ROOT / ".github" / "scripts" / "macos-signing"
ALLOW_JIT = "com.apple.security.cs.allow-jit"


class MacosSigningEntitlementsTest(unittest.TestCase):
    def load(self, binary: str) -> dict[str, bool]:
        path = SIGNING_DIR / f"{binary}.entitlements.plist"
        with path.open("rb") as file:
            return plistlib.load(file)

    def test_release_binaries_only_allow_jit(self) -> None:
        for binary in ["codex", "codex-app-server", "codex-responses-api-proxy"]:
            with self.subTest(binary=binary):
                self.assertEqual(self.load(binary), {ALLOW_JIT: True})


if __name__ == "__main__":
    unittest.main()
