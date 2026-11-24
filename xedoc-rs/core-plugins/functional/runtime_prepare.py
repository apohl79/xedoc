#!/usr/bin/env python3
"""Exercise production plugin-manager preparation with isolated local Git state.

From the repository root:
    bazel build //xedoc-rs/core-plugins:runtime-prepare-functional-probe
    python3 xedoc-rs/core-plugins/functional/runtime_prepare.py \\
        bazel-bin/xedoc-rs/core-plugins/runtime-prepare-functional-probe

The scoped probe uses production manager APIs. Its list operation also waits for
the discovery refresh worker and reads plugin details; ordinary CLI list alone
does not cover that background refresh boundary.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    binary = str(Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix="xedoc-prepare-functional-") as directory:
        root = Path(directory)
        home = root / "home"
        source = root / "source"
        plugin = source / "plugins/sample"
        home.mkdir()
        (plugin / ".xedoc-plugin").mkdir(parents=True)
        (source / ".agents/plugins").mkdir(parents=True)
        log = root / "preparation.log"
        environment = {
            "PATH": os.environ["PATH"],
            "HOME": str(home),
            "XEDOC_HOME": str(home),
            "PREPARE_LOG": str(log),
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": f"url.{source.as_uri()}.insteadOf",
            "GIT_CONFIG_VALUE_0": "https://prepare-fixture.invalid/repo.git",
            "GIT_AUTHOR_NAME": "Functional fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "Functional fixture",
            "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        }
        (home / "config.toml").write_text("[features]\nplugins = true\n")
        (source / ".agents/plugins/marketplace.json").write_text(
            json.dumps(
                {
                    "name": "prepare-fixture",
                    "plugins": [
                        {
                            "name": "sample",
                            "source": {
                                "source": "local",
                                "path": "./plugins/sample",
                            },
                        }
                    ],
                }
            )
        )
        manifest = {
            "name": "sample",
            "version": "1.0.0",
            "gui": {"prepare": {"entrypoint": "./prepare.mjs", "interpreter": "node"}},
        }

        def run(*arguments, success=True):
            result = subprocess.run(
                arguments,
                cwd=source,
                env=environment,
                capture_output=True,
                text=True,
                timeout=240,
            )
            assert (result.returncode == 0) == success, (
                arguments,
                result.stdout,
                result.stderr,
            )
            return result

        def commit_preparation(body):
            (plugin / ".xedoc-plugin/plugin.json").write_text(json.dumps(manifest))
            (plugin / "prepare.mjs").write_text(body)
            run("git", "add", ".")
            run("git", "commit", "-qm", "Fixture revision")

        def count():
            return len(log.read_text().splitlines()) if log.exists() else 0

        def preparation(revision):
            return (
                "import fs from 'node:fs';"
                f"fs.appendFileSync(process.env.PREPARE_LOG, '{revision}\\n');"
                f"fs.writeFileSync('prepared.txt', '{revision}');"
                "fs.mkdirSync('runtime', { recursive: true });"
                f"fs.writeFileSync('runtime/generated.mjs', \"console.log('{revision}')\");"
                "fs.symlinkSync('./generated.mjs', 'runtime/entry.mjs');"
                "console.log('this must not enter RPC stdout');"
            )

        run("git", "init", "-q")
        (source / ".gitignore").write_text("prepared.txt\n")
        commit_preparation(preparation("install"))
        run(
            binary,
            "plugin",
            "marketplace",
            "add",
            "https://prepare-fixture.invalid/repo.git",
        )
        run(binary, "plugin", "list", "--available", "--json")
        assert count() == 0, "marketplace/catalog discovery executed preparation"
        result = run(binary, "plugin", "add", "sample@prepare-fixture", "--json")
        json.loads(result.stdout)
        assert count() == 1
        cache = home / "plugins/cache/prepare-fixture/sample/1.0.0"
        assert (cache / "prepared.txt").read_text() == "install"
        result = run("node", str(cache / "runtime/entry.mjs"))
        assert result.stdout.strip() == "install"
        run(binary, "plugin", "list", "--available", "--json")
        assert count() == 1, "catalog listing reran preparation"
        print("PASS install prepares and copies generated symlinks; catalog does not prepare")

        commit_preparation(preparation("update"))
        result = run(
            binary, "plugin", "marketplace", "upgrade", "prepare-fixture", "--json"
        )
        assert not json.loads(result.stdout)["errors"], result.stdout
        assert count() == 2
        assert (cache / "prepared.txt").read_text() == "update"
        print("PASS force-update prepares even when plugin version is unchanged")

        config = home / "config.toml"
        config.write_text(
            config.read_text().replace("enabled = true", "enabled = false")
        )
        commit_preparation(preparation("disabled"))
        result = run(
            binary, "plugin", "marketplace", "upgrade", "prepare-fixture", "--json"
        )
        assert not json.loads(result.stdout)["errors"], result.stdout
        assert count() == 2, "disabled plugin executed preparation"
        assert (cache / "prepared.txt").read_text() == "update"
        print("PASS disabled update does not prepare or replace the ready cache")

        config.write_text(
            config.read_text().replace("enabled = false", "enabled = true")
        )
        previous = (cache / "prepare.mjs").read_bytes()
        manifest["version"] = "1.1.0"
        commit_preparation("console.error('fixture setup failed'); process.exit(7);")
        result = run(
            binary,
            "plugin",
            "marketplace",
            "upgrade",
            "prepare-fixture",
            "--json",
            success=False,
        )
        assert "preparation" in result.stderr
        assert (cache / "prepare.mjs").read_bytes() == previous
        run(binary, "plugin", "list", "--available", "--json")
        assert count() == 2, "discovery executed preparation after a failed upgrade"
        assert not (cache.parent / "1.1.0").exists()
        assert (cache / "prepare.mjs").read_bytes() == previous
        print("PASS discovery cannot bypass failed new-version preparation")
        result = run(binary, "plugin", "add", "sample@prepare-fixture", success=False)
        assert "preparation" in result.stderr
        assert (cache / "prepare.mjs").read_bytes() == previous
        print("PASS failed install/update retains previous installed cache")

        commit_preparation(preparation("retry"))
        result = run(
            binary, "plugin", "marketplace", "upgrade", "prepare-fixture", "--json"
        )
        assert not json.loads(result.stdout)["errors"], result.stdout
        assert count() == 3
        cache = cache.parent / "1.1.0"
        assert (cache / "prepared.txt").read_text() == "retry"
        previous = (cache / "prepare.mjs").read_bytes()
        print("PASS successful retry activates the prepared new version")

        manifest["gui"]["prepare"]["entrypoint"] = "./../escape.mjs"
        (source / "plugins/escape.mjs").write_text(preparation("escape"))
        commit_preparation(preparation("unused"))
        result = run(
            binary,
            "plugin",
            "marketplace",
            "upgrade",
            "prepare-fixture",
            "--json",
            success=False,
        )
        assert "inside the plugin" in result.stderr
        assert count() == 3
        assert (cache / "prepare.mjs").read_bytes() == previous
        print("PASS escaped preparation entrypoint is rejected without execution")

        manifest["version"] = "1.2.0"
        manifest["gui"]["prepare"]["entrypoint"] = "./prepare.mjs"
        commit_preparation(preparation("external-symlink"))
        outside = source / "outside.mjs"
        outside.write_text("console.log('outside')")
        external_link = plugin / "runtime/external.mjs"
        external_link.parent.mkdir(exist_ok=True)
        external_link.symlink_to(os.path.relpath(outside, external_link.parent))
        run("git", "add", ".")
        run("git", "commit", "-qm", "Fixture escaping symlink")
        result = run(
            binary,
            "plugin",
            "marketplace",
            "upgrade",
            "prepare-fixture",
            "--json",
            success=False,
        )
        assert "outside the plugin" in result.stderr, result.stderr
        assert count() == 4
        assert (cache / "prepare.mjs").read_bytes() == previous
        assert not (cache.parent / "1.2.0").exists()
        print("PASS escaping plugin symlink is rejected without replacing cache")

        run("git", "rm", "plugins/sample/runtime/external.mjs")
        manifest["version"] = "1.3.0"
        commit_preparation(preparation("escape-reentry"))
        reentry_link = plugin / "entry.mjs"
        reentry_link.symlink_to("../sample/runtime/generated.mjs")
        run("git", "add", ".")
        run("git", "commit", "-qm", "Fixture escape-and-reentry symlink")
        run(binary, "plugin", "marketplace", "upgrade", "prepare-fixture", "--json")
        assert count() == 5
        cache = cache.parent / "1.3.0"
        result = run("node", str(cache / "entry.mjs"))
        assert result.stdout.strip() == "escape-reentry"
        print("PASS escape-and-reentry symlink is rewritten for installed cache")

        run("git", "rm", "plugins/sample/entry.mjs")
        manifest["version"] = "1.4.0"
        commit_preparation(preparation("intermediate-alias"))
        (plugin / "alias").symlink_to(".", target_is_directory=True)
        (plugin / "entry.mjs").symlink_to(
            "alias/../sample/runtime/generated.mjs"
        )
        (plugin / "tools/nested").mkdir(parents=True)
        (plugin / "tools/nested/entry.mjs").symlink_to(
            "../../runtime/generated.mjs"
        )
        run("git", "add", ".")
        run("git", "commit", "-qm", "Fixture intermediate directory symlink")
        run(binary, "plugin", "marketplace", "upgrade", "prepare-fixture", "--json")
        cache = cache.parent / "1.4.0"
        result = run("node", str(cache / "entry.mjs"))
        assert result.stdout.strip() == "intermediate-alias"
        assert (cache / "alias").resolve() == cache.resolve()
        assert (cache / "entry.mjs").resolve() == (
            cache / "runtime/generated.mjs"
        ).resolve()
        result = run("node", str(cache / "tools/nested/entry.mjs"))
        assert result.stdout.strip() == "intermediate-alias"
        print("PASS intermediate directory symlink is portable after installation")

if __name__ == "__main__":
    main()
