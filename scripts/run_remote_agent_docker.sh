#!/usr/bin/env bash

# Build and run a packaged Linux Xedoc app-server with a root remote-agent workspace.
#
# Examples:
#   scripts/run_remote_agent_docker.sh
#   scripts/run_remote_agent_docker.sh --target linux-x86_64
#   scripts/run_remote_agent_docker.sh --package dist/xedoc/1.44.0/xedoc-aarch64-unknown-linux-gnu-1.44.0.zip

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd "$script_dir/.." && pwd)"
readonly repo_root
readonly dockerfile="$script_dir/remote_agent_docker/Dockerfile"
readonly container_label="com.xedoc.remote-agent-docker-test"
readonly host_auth_path="$HOME/.xedoc/auth.json"
readonly host_secrets_path="$HOME/.xedoc/secrets"

container_name="xedoc-linux-remote-agent"
package_zip=""
target=""

usage() {
  cat <<'EOF'
Usage: scripts/run_remote_agent_docker.sh [options]

Builds a Linux release package into a Docker image, starts its app-server
daemon, and verifies the bundled remote-agent broker. The container configures
the coordinator role with workspace ID `root` mapped to `/`.

Options:
  --package PATH             Linux Xedoc release ZIP to install.
  --target TARGET            linux-arm64 or linux-x86_64. Defaults to the
                             running Docker server architecture.
  --name NAME                Container name. Default: xedoc-linux-remote-agent.
  -h, --help                 Show this help.

After a successful run:
  docker exec -it xedoc-linux-remote-agent bash
  docker exec xedoc-linux-remote-agent xedoc app-server daemon version
  docker exec xedoc-linux-remote-agent xedoc-remote-agentd doctor
EOF
}

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

target_triple() {
  case "$1" in
    linux-arm64) printf '%s\n' 'aarch64-unknown-linux-gnu' ;;
    linux-x86_64) printf '%s\n' 'x86_64-unknown-linux-gnu' ;;
    *) fail "unsupported target: $1 (use linux-arm64 or linux-x86_64)" ;;
  esac
}

target_platform() {
  case "$1" in
    linux-arm64) printf '%s\n' 'linux/arm64' ;;
    linux-x86_64) printf '%s\n' 'linux/amd64' ;;
    *) fail "unsupported target: $1 (use linux-arm64 or linux-x86_64)" ;;
  esac
}

default_target() {
  case "$(docker version --format '{{.Server.Arch}}')" in
    aarch64|arm64) printf '%s\n' 'linux-arm64' ;;
    x86_64|amd64) printf '%s\n' 'linux-x86_64' ;;
    *) fail "unsupported Docker server architecture" ;;
  esac
}

find_package() {
  local triple="$1"
  python3 - "$repo_root" "$triple" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])
triple = sys.argv[2]
candidates = list((root / "dist" / "xedoc").glob(f"*/xedoc-{triple}-*.zip"))
if not candidates:
    raise SystemExit(1)
print(max(candidates, key=lambda path: path.stat().st_mtime))
PY
}

package_target() {
  python3 - "$1" <<'PY'
import json
from pathlib import Path
import sys
from zipfile import ZipFile

with ZipFile(Path(sys.argv[1])) as archive:
    manifest = json.loads(archive.read("xedoc-package.json"))
target = manifest.get("target")
if not isinstance(target, str):
    raise SystemExit(1)
print(target)
PY
}

while (($#)); do
  case "$1" in
    --package)
      (($# >= 2)) || fail "--package requires a path"
      package_zip="$2"
      shift 2
      ;;
    --target)
      (($# >= 2)) || fail "--target requires a value"
      target="$2"
      shift 2
      ;;
    --name)
      (($# >= 2)) || fail "--name requires a value"
      container_name="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

[[ -f "$dockerfile" ]] || fail "Dockerfile is missing: $dockerfile"
[[ "$container_name" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] ||
  fail "invalid container name: $container_name"
[[ -f "$host_auth_path" ]] ||
  fail "Xedoc authentication file does not exist: $host_auth_path"
[[ -d "$host_secrets_path" ]] ||
  fail "Xedoc secrets directory does not exist: $host_secrets_path"

if [[ -z "${DOCKER_HOST:-}" && -S "$HOME/.colima/default/docker.sock" ]]; then
  export DOCKER_HOST="unix://${HOME}/.colima/default/docker.sock"
fi
docker info >/dev/null || fail "Docker is unavailable; start Colima with: colima start"

if [[ -z "$target" ]]; then
  target="$(default_target)"
fi
triple="$(target_triple "$target")"
platform="$(target_platform "$target")"

if [[ -z "$package_zip" ]]; then
  package_zip="$(find_package "$triple")" ||
    fail "no package for $triple; build one or pass --package"
fi
package_zip="$(cd "$(dirname "$package_zip")" && pwd)/$(basename "$package_zip")"
[[ -f "$package_zip" ]] || fail "package does not exist: $package_zip"
package_target_value="$(package_target "$package_zip")" ||
  fail "package has no valid xedoc-package.json target: $package_zip"
[[ "$package_target_value" == "$triple" ]] ||
  fail "package target $package_target_value does not match $target ($triple)"

build_context="$(mktemp -d "${TMPDIR:-/tmp}/xedoc-remote-agent-docker.XXXXXX")"
cleanup() {
  rm -rf "$build_context"
}
trap cleanup EXIT

ln "$package_zip" "$build_context/package.zip" 2>/dev/null ||
  cp "$package_zip" "$build_context/package.zip"

image_name="${container_name}:latest"
printf '==> Building %s from %s for %s\n' "$image_name" "$package_zip" "$platform"
docker build \
  --platform "$platform" \
  --tag "$image_name" \
  --file "$dockerfile" \
  "$build_context"

if docker container inspect "$container_name" >/dev/null 2>&1; then
  existing_label="$(
    docker container inspect \
      --format "{{ index .Config.Labels \"$container_label\" }}" \
      "$container_name"
  )"
  [[ "$existing_label" == "true" ]] ||
    fail "container already exists and is not owned by this harness: $container_name"
  docker rm -f "$container_name" >/dev/null
fi
printf '==> Starting %s\n' "$container_name"
docker run -d \
  --platform "$platform" \
  --name "$container_name" \
  --label "$container_label=true" \
  --mount "type=bind,src=$host_auth_path,dst=/root/.xedoc/auth.json,readonly" \
  --mount "type=bind,src=$host_secrets_path,dst=/root/.xedoc/secrets,readonly" \
  "$image_name" >/dev/null

printf '==> Starting packaged app-server daemon\n'
docker exec "$container_name" xedoc app-server daemon start
printf '==> Verifying daemon and remote-agent broker\n'
docker exec "$container_name" xedoc app-server daemon version
docker exec "$container_name" xedoc-remote-agentd status
docker exec "$container_name" xedoc-remote-agentd doctor

cat <<EOF
==> Ready
Container: $container_name
Workspace: root -> /
Shell:     docker exec -it $container_name bash
Daemon:    docker exec $container_name xedoc app-server daemon version
Broker:    docker exec $container_name xedoc-remote-agentd doctor
Stop:      docker rm -f $container_name
EOF
