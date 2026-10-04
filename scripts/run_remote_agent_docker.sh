#!/usr/bin/env bash
# Manage a packaged Linux Xedoc managed remote-agent peer.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
dockerfile="$script_dir/remote_agent_docker/Dockerfile"
container_label="com.xedoc.remote-agent-docker-test"
container_name="xedoc-linux-remote-agent"
package_zip=""
target=""
normal_port=46000
pairing_port=46001
discovery_port=43371
advertise_host=""
command="start"
command_set=0

usage() {
  cat <<'EOF'
Usage: scripts/run_remote_agent_docker.sh [start|stop] [options]

`start` (the default) builds and starts a managed Linux peer with workspace
`root` mapped to `/`. It does not trust a copied coordinator certificate: the
printed, one-time enrollment code is the only automatic managed-pairing
authorization.

`stop` removes the managed-peer container, its harness image, and dangling
Docker image layers to reclaim Colima disk space.

Options:
  --package PATH             Linux Xedoc package ZIP (default: newest matching dist ZIP).
  --target TARGET            linux-arm64 or linux-x86_64 (default: Docker server architecture).
  --name NAME                Container name (default: xedoc-linux-remote-agent).
  --normal-port PORT         Host TCP port for peer traffic (default: 46000).
  --pairing-port PORT        Host TCP port for pairing traffic (default: 46001).
  --discovery-port PORT      Host UDP direct-discovery port (default: 43371).
  --advertise-host IPV4      Non-loopback IPv4 address advertised to coordinators.
                             Default: this machine's outbound IPv4 address.
  -h, --help                 Show this help.
EOF
}
fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
configure_docker() {
  if [[ -z "${DOCKER_HOST:-}" && -S "$HOME/.colima/default/docker.sock" ]]; then
    export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"
  fi
  if ! docker info >/dev/null 2>&1 && command -v colima >/dev/null 2>&1; then
    colima start
  fi
  docker info >/dev/null || fail "Docker is unavailable; start Colima with: colima start"
}
target_triple() { case "$1" in linux-arm64) echo aarch64-unknown-linux-gnu;; linux-x86_64) echo x86_64-unknown-linux-gnu;; *) fail "unsupported target: $1";; esac; }
target_platform() { case "$1" in linux-arm64) echo linux/arm64;; linux-x86_64) echo linux/amd64;; *) fail "unsupported target: $1";; esac; }
default_target() { case "$(docker version --format '{{.Server.Arch}}')" in aarch64|arm64) echo linux-arm64;; x86_64|amd64) echo linux-x86_64;; *) fail "unsupported Docker server architecture";; esac; }
valid_port() { [[ "$1" =~ ^[1-9][0-9]{0,4}$ ]] && (( "$1" <= 65535 )); }
outbound_ipv4() {
  python3 - <<'PY'
import ipaddress
import socket

with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
    probe.connect(("8.8.8.8", 80))
    address = probe.getsockname()[0]
parsed = ipaddress.ip_address(address)
if parsed.is_loopback or parsed.is_unspecified:
    raise SystemExit("no non-loopback outbound IPv4 address is available")
print(address)
PY
}
validate_advertise_host() {
  python3 - "$1" <<'PY'
import ipaddress
import socket
import sys

host = sys.argv[1]
try:
    parsed = ipaddress.ip_address(host)
except ValueError as error:
    raise SystemExit(f"advertised host must be an IPv4 address: {host!r}") from error
if parsed.version != 4 or parsed.is_loopback or parsed.is_unspecified:
    raise SystemExit(f"advertised host must be a non-loopback IPv4 address: {host!r}")
PY
}
find_package() {
  python3 - "$repo_root" "$1" <<'PY'
from pathlib import Path
import sys
items = list((Path(sys.argv[1]) / "dist" / "xedoc").glob(f"*/xedoc-{sys.argv[2]}-*.zip"))
if not items: raise SystemExit(1)
print(max(items, key=lambda p: p.stat().st_mtime))
PY
}
package_target() {
  python3 - "$1" <<'PY'
import json, sys
from zipfile import ZipFile
with ZipFile(sys.argv[1]) as z: print(json.loads(z.read("xedoc-package.json"))["target"])
PY
}

while (($#)); do
  case "$1" in
    start|stop)
      (( command_set == 0 )) || fail "command specified more than once"
      command="$1"
      command_set=1
      shift;;
    --package|--target|--name|--normal-port|--pairing-port|--discovery-port|--advertise-host)
      (($# >= 2)) || fail "$1 requires a value"
      case "$1" in --package) package_zip="$2";; --target) target="$2";; --name) container_name="$2";; --normal-port) normal_port="$2";; --pairing-port) pairing_port="$2";; --discovery-port) discovery_port="$2";; *) advertise_host="$2";; esac
      shift 2;;
    -h|--help) usage; exit 0;;
    *) fail "unknown option: $1";;
  esac
done
[[ "$container_name" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || fail "invalid container name"
configure_docker
image_name="${container_name}:latest"
if [[ "$command" == stop ]]; then
  if docker container inspect "$container_name" >/dev/null 2>&1; then
    label="$(docker container inspect --format "{{ index .Config.Labels \"$container_label\" }}" "$container_name")"
    [[ "$label" == true ]] || fail "container exists and is not owned by this harness: $container_name"
    docker rm -f "$container_name" >/dev/null
    printf 'Removed managed peer container: %s\n' "$container_name"
  else
    printf 'Managed peer container is not running: %s\n' "$container_name"
  fi
  if docker image inspect "$image_name" >/dev/null 2>&1; then
    label="$(docker image inspect --format "{{ index .Config.Labels \"$container_label\" }}" "$image_name")"
    if [[ "$label" == true ]]; then
      docker image rm -f "$image_name" >/dev/null
      printf 'Removed harness image: %s\n' "$image_name"
    else
      printf 'Preserved image not owned by this harness: %s\n' "$image_name" >&2
    fi
  fi
  docker image prune -f >/dev/null
  printf 'Pruned dangling Docker image layers.\n'
  exit 0
fi
for port in "$normal_port" "$pairing_port" "$discovery_port"; do valid_port "$port" || fail "invalid port: $port"; done
[[ -f "$HOME/.xedoc/auth.json" ]] || fail "missing ~/.xedoc/auth.json"
[[ -d "$HOME/.xedoc/secrets" ]] || fail "missing ~/.xedoc/secrets"
if [[ -z "$advertise_host" ]]; then
  advertise_host="$(outbound_ipv4)" || fail "could not determine a non-loopback outbound IPv4 address; pass --advertise-host"
fi
validate_advertise_host "$advertise_host" || fail "invalid --advertise-host: $advertise_host"
[[ -n "$target" ]] || target="$(default_target)"
triple="$(target_triple "$target")"; platform="$(target_platform "$target")"
[[ -n "$package_zip" ]] || package_zip="$(find_package "$triple")" || fail "no package for $triple; pass --package"
package_zip="$(cd "$(dirname "$package_zip")" && pwd)/$(basename "$package_zip")"
[[ -f "$package_zip" ]] || fail "package does not exist: $package_zip"
[[ "$(package_target "$package_zip")" == "$triple" ]] || fail "package target does not match $target"

build_context="$(mktemp -d "${TMPDIR:-/tmp}/xedoc-remote-agent-docker.XXXXXX")"
trap 'rm -rf "$build_context"' EXIT
cp "$package_zip" "$build_context/package.zip"
docker build --platform "$platform" --label "$container_label=true" --tag "$image_name" --file "$dockerfile" "$build_context"
if docker container inspect "$container_name" >/dev/null 2>&1; then
  label="$(docker container inspect --format "{{ index .Config.Labels \"$container_label\" }}" "$container_name")"
  [[ "$label" == true ]] || fail "container already exists and is not owned by this harness: $container_name"
  docker rm -f "$container_name" >/dev/null
fi
docker run -d --platform "$platform" --name "$container_name" --label "$container_label=true" \
  --add-host host.docker.internal:host-gateway \
  -p "0.0.0.0:${normal_port}:46000/tcp" -p "0.0.0.0:${pairing_port}:46001/tcp" \
  -p "127.0.0.1:${discovery_port}:43371/udp" \
  --mount "type=bind,src=$HOME/.xedoc/auth.json,dst=/root/.xedoc/auth.json,readonly" \
  --mount "type=bind,src=$HOME/.xedoc/secrets,dst=/root/.xedoc/secrets,readonly" \
  "$image_name" >/dev/null
docker exec "$container_name" sh -ceu "cat > /root/.xedoc/config.toml <<'EOF'
[remote_agent]
role = \"managed\"
workspaces = { root = \"/\" }
peer_listener = { endpoint = \"tls://0.0.0.0:46000\", pairing_endpoint = \"tls://0.0.0.0:46001\", advertised_endpoint = \"tls://${advertise_host}:${normal_port}\", advertised_pairing_endpoint = \"tls://${advertise_host}:${pairing_port}\" }
[remote_agent.limits]
max_attached_sessions = 8
max_message_bytes = 16384
max_result_bytes = 65536
max_wait_seconds = 120
max_discovery_seconds = 10
audit_retention_days = 90
EOF
mkdir -p /root/.xedoc/remote-agent
printf '%s\n' 'controller = \"unix:///root/.xedoc/app-server-control/app-server-control.sock\"' > /root/.xedoc/remote-agent/bootstrap.toml
chmod 700 /root/.xedoc/remote-agent
chmod 600 /root/.xedoc/remote-agent/bootstrap.toml"
docker exec "$container_name" xedoc app-server daemon start
docker exec "$container_name" xedoc app-server daemon version
code="$(docker exec "$container_name" xedoc-remote-agentd enrollment create | python3 -c 'import json,sys; print(json.load(sys.stdin)["code"])')"
cat <<EOF
==> Ready: managed peer $container_name
Enrollment code: $code
Pair from the coordinator:
  xedoc-session remote-agent-enroll --endpoint 127.0.0.1:$discovery_port
Container shell: docker exec -it $container_name bash
Stop and reclaim harness image space: scripts/run_remote_agent_docker.sh stop --name $container_name
EOF
