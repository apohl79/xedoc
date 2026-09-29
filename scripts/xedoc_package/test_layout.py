#!/usr/bin/env python3

from pathlib import Path
import json
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from xedoc_package.layout import build_package_dir
from xedoc_package.layout import validate_package_dir
from xedoc_package.model_router_runtime import RuntimeReference
from xedoc_package.remote_agent_runtime import RemoteAgentRuntimeReference
from xedoc_package.remote_agent_runtime import hash_tree
from xedoc_package.targets import PACKAGE_VARIANTS
from xedoc_package.targets import PackageInputs
from xedoc_package.targets import TARGET_SPECS


class PackageLayoutTest(unittest.TestCase):
    def test_default_package_layout_excludes_session_control(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = root / "package"
            package_dir.mkdir()
            inputs = PackageInputs(
                entrypoint_bin=touch_executable(root / "xedoc"),
                rg_bin=touch_executable(root / "rg"),
                bwrap_bin=touch_executable(root / "bwrap"),
            )
            remote_agent_runtime = remote_agent_runtime_reference(root)

            build_package_dir(
                package_dir,
                "1.2.3",
                PACKAGE_VARIANTS["xedoc"],
                TARGET_SPECS["x86_64-unknown-linux-gnu"],
                inputs,
                model_router_runtime=runtime_reference(),
                remote_agent_runtime=remote_agent_runtime,
            )
            validate_package_dir(
                package_dir,
                PACKAGE_VARIANTS["xedoc"],
                TARGET_SPECS["x86_64-unknown-linux-gnu"],
                model_router_runtime=runtime_reference(),
                remote_agent_runtime=remote_agent_runtime,
            )

            metadata = json.loads(
                (package_dir / "xedoc-package.json").read_text(encoding="utf-8")
            )
            self.assertEqual(
                {
                    "layout_version": metadata["layoutVersion"],
                    "session_control": (package_dir / "bin" / "xedoc-session").exists(),
                },
                {"layout_version": 3, "session_control": False},
            )
            self.assertEqual(
                metadata["remoteAgent"]["launcher"],
                "bin/xedoc-remote-agentd",
            )
            self.assertTrue(
                (
                    package_dir
                    / "xedoc-resources/remote-agent/runtime/python/bin/python3"
                ).is_file()
            )

    def test_app_server_package_places_session_control_beside_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = root / "package"
            package_dir.mkdir()
            inputs = PackageInputs(
                entrypoint_bin=touch_executable(root / "xedoc-app-server"),
                rg_bin=touch_executable(root / "rg"),
                bwrap_bin=touch_executable(root / "bwrap"),
            )
            remote_agent_runtime = remote_agent_runtime_reference(root)

            build_package_dir(
                package_dir,
                "1.2.3",
                PACKAGE_VARIANTS["xedoc-app-server"],
                TARGET_SPECS["x86_64-unknown-linux-gnu"],
                inputs,
                model_router_runtime=runtime_reference(),
                remote_agent_runtime=remote_agent_runtime,
                include_session_control=True,
            )
            validate_package_dir(
                package_dir,
                PACKAGE_VARIANTS["xedoc-app-server"],
                TARGET_SPECS["x86_64-unknown-linux-gnu"],
                model_router_runtime=runtime_reference(),
                remote_agent_runtime=remote_agent_runtime,
                include_session_control=True,
            )

            self.assertEqual(
                {
                    name: (package_dir / "bin" / name).is_file()
                    for name in ("xedoc-app-server", "xedoc-session")
                },
                {"xedoc-app-server": True, "xedoc-session": True},
            )
            metadata = json.loads(
                (package_dir / "xedoc-package.json").read_text(encoding="utf-8")
            )
            self.assertEqual(metadata["layoutVersion"], 5)


def touch_executable(path: Path) -> Path:
    path.touch(mode=0o755)
    return path


def runtime_reference() -> RuntimeReference:
    return RuntimeReference(
        runtime_id="r1-sha256-test",
        asset_name="xedoc-model-router-runtime-x86_64-unknown-linux-gnu-r1-sha256-test.zip",
        sha256="0" * 64,
        source_release_tag="model-router-runtime-r1-sha256-test",
    )


def remote_agent_runtime_reference(root: Path) -> RemoteAgentRuntimeReference:
    runtime_root = root / "remote-agent-runtime" / "python"
    interpreter = runtime_root / "bin" / "python3"
    interpreter.parent.mkdir(parents=True)
    touch_executable(interpreter)
    return RemoteAgentRuntimeReference(
        target="x86_64-unknown-linux-gnu",
        runtime_id="remote-agent-python-r1-test",
        python_version="3.12.14",
        runtime_sha256=hash_tree(runtime_root),
        root=runtime_root,
    )


if __name__ == "__main__":
    unittest.main()
