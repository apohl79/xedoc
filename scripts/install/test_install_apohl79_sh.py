#!/usr/bin/env python3

import errno
import hashlib
import json
import os
from pathlib import Path
import pty
import subprocess
import tempfile
import textwrap
import unittest
import zipfile


INSTALL_SCRIPT = Path(__file__).with_name("install-apohl79.sh")
TAG = "rust-v0.144.0-apohl79-17"
VERSION = TAG.removeprefix("rust-v")
TARGET = "aarch64-apple-darwin"
ASSET = f"codex-{TARGET}-{VERSION}.zip"


class InstallApohl79ShTest(unittest.TestCase):
    def test_package_install_creates_visible_codex_and_host_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TAG": TAG,
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )

            result = subprocess.run(
                ["/bin/sh", str(INSTALL_SCRIPT)],
                capture_output=True,
                check=False,
                env=env,
                text=True,
            )

            install_bin = root / "install-bin"
            release_dir = (
                root
                / "codex-home"
                / "packages"
                / "standalone"
                / "releases"
                / f"{VERSION}-{TARGET}"
            )
            self.assertEqual(
                {
                    "returncode": result.returncode,
                    "codex_link": os.readlink(install_bin / "codex"),
                    "host_link": os.readlink(install_bin / "codex-code-mode-host"),
                    "session_link": os.readlink(install_bin / "codex-session"),
                    "host_installed": (
                        release_dir / "bin/codex-code-mode-host"
                    ).is_file(),
                },
                {
                    "returncode": 0,
                    "codex_link": str(
                        root / "codex-home/packages/standalone/current/bin/codex"
                    ),
                    "host_link": str(
                        root
                        / "codex-home/packages/standalone/current/bin/codex-code-mode-host"
                    ),
                    "session_link": str(
                        root
                        / "codex-home/packages/standalone/current/bin/codex-session"
                    ),
                    "host_installed": True,
                },
            )

    def test_saved_zshrc_choice_adds_app_server_startup(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")
            choice_path = root / "codex-home/app-server-daemon/zshrc-start"
            choice_path.parent.mkdir(parents=True)
            choice_path.write_text("enabled\n", encoding="utf-8")

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TAG": TAG,
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )
            (root / "home").mkdir()

            result = subprocess.run(
                ["/bin/sh", str(INSTALL_SCRIPT)],
                capture_output=True,
                check=False,
                env=env,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            zshrc = (root / "home/.zshrc").read_text(encoding="utf-8")
            expected_binary = root / "install-bin/codex"
            self.assertIn(
                f'"{expected_binary}" app-server daemon start',
                zshrc,
            )

    def test_streamed_installer_uses_the_latest_fork_release(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")
            request_log = root / "requests.log"

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "CODEX_TEST_REQUEST_LOG": str(request_log),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )

            result = subprocess.run(
                ["/bin/sh"],
                capture_output=True,
                check=False,
                cwd=root,
                env=env,
                input=INSTALL_SCRIPT.read_text(encoding="utf-8"),
                text=True,
            )

            self.assertEqual(
                {
                    "returncode": result.returncode,
                    "requests": request_log.read_text(encoding="utf-8").splitlines(),
                    "codex_link": os.readlink(root / "install-bin/codex"),
                    "statusline": (root / "codex-home/statusline.sh").read_text(
                        encoding="utf-8"
                    ),
                },
                {
                    "returncode": 0,
                    "requests": [
                        "https://api.github.com/repos/apohl79/codex/releases/latest",
                        f"https://api.github.com/repos/apohl79/codex/releases/tags/{TAG}",
                        f"https://github.com/apohl79/codex/releases/download/{TAG}/{ASSET}",
                        f"https://raw.githubusercontent.com/apohl79/codex/{TAG}/scripts/statusline.sh",
                    ],
                    "codex_link": str(
                        root / "codex-home/packages/standalone/current/bin/codex"
                    ),
                    "statusline": "#!/bin/sh\n",
                },
            )

    def test_existing_statusline_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")
            existing_statusline = root / "codex-home/statusline.sh"
            existing_statusline.parent.mkdir(parents=True)
            existing_statusline.write_text(
                "#!/bin/sh\n# user customization\n",
                encoding="utf-8",
            )

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TAG": TAG,
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )

            result = subprocess.run(
                ["/bin/sh", str(INSTALL_SCRIPT)],
                capture_output=True,
                check=False,
                env=env,
                text=True,
            )

            self.assertEqual(
                {
                    "returncode": result.returncode,
                    "statusline": existing_statusline.read_text(encoding="utf-8"),
                    "downloaded_statusline": "Downloading statusline script"
                    in result.stdout,
                },
                {
                    "returncode": 0,
                    "statusline": "#!/bin/sh\n# user customization\n",
                    "downloaded_statusline": False,
                },
            )

    def test_accepting_codex_providers_prompt_runs_official_installer(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")
            app_server_choice_path = root / "codex-home/app-server-daemon/zshrc-start"
            app_server_choice_path.parent.mkdir(parents=True)
            app_server_choice_path.write_text("disabled\n", encoding="utf-8")
            provider_installer = root / "codex-providers-install.sh"
            provider_marker = root / "provider-installed"
            write_fake_provider_installer(provider_installer)
            request_log = root / "requests.log"

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TAG": TAG,
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "CODEX_TEST_PROVIDER_INSTALLER": str(provider_installer),
                    "CODEX_TEST_PROVIDER_INSTALL_MARKER": str(provider_marker),
                    "CODEX_TEST_REQUEST_LOG": str(request_log),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )

            result = run_interactive_installer(env, "y\n")

            self.assertEqual(
                {
                    "returncode": result.returncode,
                    "requests": request_log.read_text(encoding="utf-8").splitlines(),
                    "provider_marker": provider_marker.read_text(encoding="utf-8"),
                    "provider_runner": (
                        root / "home/.local/bin/codex-providers"
                    ).is_file(),
                    "provider_choice_exists": (
                        root / "codex-home/codex-providers/install"
                    ).exists(),
                },
                {
                    "returncode": 0,
                    "requests": [
                        f"https://api.github.com/repos/apohl79/codex/releases/tags/{TAG}",
                        f"https://github.com/apohl79/codex/releases/download/{TAG}/{ASSET}",
                        "https://raw.githubusercontent.com/apohl79/codex-providers/main/install.sh",
                    ],
                    "provider_marker": "installed\n",
                    "provider_runner": True,
                    "provider_choice_exists": False,
                },
            )

    def test_rejected_codex_providers_prompt_is_not_asked_again(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")
            app_server_choice_path = root / "codex-home/app-server-daemon/zshrc-start"
            app_server_choice_path.parent.mkdir(parents=True)
            app_server_choice_path.write_text("disabled\n", encoding="utf-8")
            request_log = root / "requests.log"

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TAG": TAG,
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "CODEX_TEST_REQUEST_LOG": str(request_log),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )

            first_result = run_interactive_installer(env, "n\n")
            second_result = run_interactive_installer(env, "")

            self.assertEqual(
                {
                    "returncodes": [first_result.returncode, second_result.returncode],
                    "requests": request_log.read_text(encoding="utf-8").splitlines(),
                    "provider_choice": (
                        root / "codex-home/codex-providers/install"
                    ).read_text(encoding="utf-8"),
                    "provider_prompts": [
                        "Install optional codex-providers" in first_result.stdout,
                        "Install optional codex-providers" in second_result.stdout,
                    ],
                },
                {
                    "returncodes": [0, 0],
                    "requests": [
                        f"https://api.github.com/repos/apohl79/codex/releases/tags/{TAG}",
                        f"https://github.com/apohl79/codex/releases/download/{TAG}/{ASSET}",
                        f"https://api.github.com/repos/apohl79/codex/releases/tags/{TAG}",
                    ],
                    "provider_choice": "disabled\n",
                    "provider_prompts": [True, False],
                },
            )

    def test_existing_codex_providers_runner_is_not_prompted(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path = root / ASSET
            write_package_archive(archive_path)
            archive_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            bin_dir = root / "fake-bin"
            bin_dir.mkdir()
            write_fake_curl(bin_dir / "curl")
            app_server_choice_path = root / "codex-home/app-server-daemon/zshrc-start"
            app_server_choice_path.parent.mkdir(parents=True)
            app_server_choice_path.write_text("disabled\n", encoding="utf-8")
            provider_runner = root / "home/.local/bin/codex-providers"
            provider_runner.parent.mkdir(parents=True)
            provider_runner.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            provider_runner.chmod(0o755)
            request_log = root / "requests.log"

            env = os.environ.copy()
            env.update(
                {
                    "CODEX_APOHL79_REPO": "apohl79/codex",
                    "CODEX_APOHL79_TAG": TAG,
                    "CODEX_APOHL79_TARGET": TARGET,
                    "CODEX_HOME": str(root / "codex-home"),
                    "CODEX_INSTALL_DIR": str(root / "install-bin"),
                    "CODEX_TEST_ARCHIVE": str(archive_path),
                    "CODEX_TEST_METADATA_JSON": release_metadata(archive_digest),
                    "CODEX_TEST_REQUEST_LOG": str(request_log),
                    "HOME": str(root / "home"),
                    "PATH": f"{bin_dir}:/usr/bin:/bin",
                    "SHELL": "/bin/sh",
                }
            )

            result = run_interactive_installer(env, "")

            self.assertEqual(
                {
                    "returncode": result.returncode,
                    "requests": request_log.read_text(encoding="utf-8").splitlines(),
                    "provider_prompt_seen": "Install optional codex-providers"
                    in result.stdout,
                },
                {
                    "returncode": 0,
                    "requests": [
                        f"https://api.github.com/repos/apohl79/codex/releases/tags/{TAG}",
                        f"https://github.com/apohl79/codex/releases/download/{TAG}/{ASSET}",
                    ],
                    "provider_prompt_seen": False,
                },
            )


def run_interactive_installer(
    env: dict[str, str], response: str
) -> subprocess.CompletedProcess[str]:
    child_pid, master_fd = pty.fork()
    if child_pid == 0:
        os.execvpe("/bin/sh", ["/bin/sh", str(INSTALL_SCRIPT)], env)

    output_chunks: list[bytes] = []
    try:
        if response:
            os.write(master_fd, response.encode())
        while True:
            try:
                output = os.read(master_fd, 4096)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not output:
                break
            output_chunks.append(output)
    finally:
        os.close(master_fd)

    _, status = os.waitpid(child_pid, 0)
    return subprocess.CompletedProcess(
        ["/bin/sh", str(INSTALL_SCRIPT)],
        os.waitstatus_to_exitcode(status),
        b"".join(output_chunks).decode(),
        "",
    )


def write_package_archive(archive_path: Path) -> None:
    with zipfile.ZipFile(archive_path, "w") as archive:
        write_zip_text(archive, "codex-package.json", "{}\n")
        write_zip_text(
            archive,
            "bin/codex",
            "#!/bin/sh\nprintf 'codex-cli 0.144.0-apohl79-17\\n'\n",
            mode=0o755,
        )
        write_zip_text(
            archive,
            "bin/codex-code-mode-host",
            "#!/bin/sh\nexit 0\n",
            mode=0o755,
        )
        write_zip_text(
            archive,
            "bin/codex-session",
            "#!/bin/sh\nexit 0\n",
            mode=0o755,
        )
        write_zip_text(archive, "codex-path/rg", "#!/bin/sh\nexit 0\n", mode=0o755)


def write_zip_text(
    archive: zipfile.ZipFile,
    name: str,
    content: str,
    *,
    mode: int = 0o644,
) -> None:
    info = zipfile.ZipInfo(name)
    info.external_attr = mode << 16
    archive.writestr(info, content)


def write_fake_curl(path: Path) -> None:
    path.write_text(
        textwrap.dedent(
            """\
            #!/bin/sh
            output=""
            url=""
            while [ "$#" -gt 0 ]; do
              case "$1" in
                -o)
                  output="$2"
                  shift
                  ;;
                https://*)
                  url="$1"
                  ;;
              esac
              shift
            done

            if [ -n "${CODEX_TEST_REQUEST_LOG:-}" ]; then
              printf '%s\\n' "$url" >> "$CODEX_TEST_REQUEST_LOG"
            fi

            case "$url" in
              https://api.github.com/*)
                printf '%s\\n' "$CODEX_TEST_METADATA_JSON"
                ;;
              https://github.com/*)
                cp "$CODEX_TEST_ARCHIVE" "$output"
                ;;
              https://raw.githubusercontent.com/apohl79/codex-providers/main/install.sh)
                cat "$CODEX_TEST_PROVIDER_INSTALLER"
                ;;
              https://raw.githubusercontent.com/*)
                printf '#!/bin/sh\\n' > "$output"
                ;;
              *)
                exit 22
                ;;
            esac
            """
        ),
        encoding="utf-8",
    )
    path.chmod(0o755)


def write_fake_provider_installer(path: Path) -> None:
    path.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            set -euo pipefail
            mkdir -p "$HOME/.local/bin"
            printf '#!/usr/bin/env bash\\nexit 0\\n' > "$HOME/.local/bin/codex-providers"
            chmod +x "$HOME/.local/bin/codex-providers"
            printf 'installed\\n' > "$CODEX_TEST_PROVIDER_INSTALL_MARKER"
            """
        ),
        encoding="utf-8",
    )


def release_metadata(archive_digest: str) -> str:
    return json.dumps(
        {
            "tag_name": TAG,
            "assets": [
                {
                    "name": ASSET,
                    "digest": f"sha256:{archive_digest}",
                }
            ],
        },
        indent=2,
    )


if __name__ == "__main__":
    unittest.main()
