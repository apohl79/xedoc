#!/bin/sh

set -eu

APOHL79_REPO="${CODEX_APOHL79_REPO:-apohl79/codex}"
APOHL79_TAG="${CODEX_APOHL79_TAG:-}"
APOHL79_TARGET="${CODEX_APOHL79_TARGET:-}"
BIN_DIR="${CODEX_INSTALL_DIR:-$HOME/.local/bin}"
BIN_PATH="$BIN_DIR/codex"
HOST_BIN_PATH="$BIN_DIR/codex-code-mode-host"
CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
STANDALONE_ROOT="$CODEX_HOME_DIR/packages/standalone"
RELEASES_DIR="$STANDALONE_ROOT/releases"
CURRENT_LINK="$STANDALONE_ROOT/current"
CHECK_ONLY=false
tmp_dir=""

script_dir="$(CDPATH='' cd "$(dirname "$0")" && pwd)"
repo_root="$(CDPATH='' cd "$script_dir/../.." && pwd)"

step() {
  printf '==> %s\n' "$1"
}

die() {
  printf 'Error: %s\n' "$1" >&2
  exit 1
}

usage() {
  cat <<EOF
Usage: install-apohl79.sh [--tag TAG] [--target TARGET] [--repo OWNER/REPO] [--check]

Downloads and installs the apohl79 Codex fork binary release for the current fork tag.

Options:
  --tag TAG        Fork release tag to install. Defaults to the current rust-v*-apohl79-N tag.
  --target TARGET  Release target triple. Defaults to the current platform.
  --repo OWNER/REPO
                   GitHub repository to read releases from. Defaults to apohl79/codex.
  --check          Verify that the release asset exists, then print the plan and exit.
  -h, --help       Show this help.

Environment:
  CODEX_APOHL79_TAG     Same as --tag.
  CODEX_APOHL79_TARGET  Same as --target.
  CODEX_APOHL79_REPO    Same as --repo.
  CODEX_INSTALL_DIR     Directory for the visible codex symlinks. Defaults to ~/.local/bin.
  CODEX_HOME            Codex home directory. Defaults to ~/.codex.
  GH_TOKEN/GITHUB_TOKEN Optional GitHub token for API requests.
EOF
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --tag)
        [ "$#" -ge 2 ] || die "--tag requires a value."
        APOHL79_TAG="$2"
        shift
        ;;
      --target)
        [ "$#" -ge 2 ] || die "--target requires a value."
        APOHL79_TARGET="$2"
        shift
        ;;
      --repo)
        [ "$#" -ge 2 ] || die "--repo requires a value."
        APOHL79_REPO="$2"
        shift
        ;;
      --check)
        CHECK_ONLY=true
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *)
        die "Unknown argument: $1"
        ;;
    esac
    shift
  done
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required."
}

github_token() {
  if [ -n "${GH_TOKEN:-}" ]; then
    printf '%s\n' "$GH_TOKEN"
    return
  fi
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    printf '%s\n' "$GITHUB_TOKEN"
  fi
  return 0
}

download_file() {
  url="$1"
  output="$2"
  token="$(github_token)"

  if command -v curl >/dev/null 2>&1; then
    if [ -n "$token" ]; then
      curl -fsSL -H "Authorization: Bearer $token" "$url" -o "$output"
    else
      curl -fsSL "$url" -o "$output"
    fi
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    if [ -n "$token" ]; then
      wget -q --header="Authorization: Bearer $token" -O "$output" "$url"
    else
      wget -q -O "$output" "$url"
    fi
    return
  fi

  die "curl or wget is required."
}

download_text() {
  url="$1"
  token="$(github_token)"

  if command -v curl >/dev/null 2>&1; then
    if [ -n "$token" ]; then
      curl -fsSL -H "Authorization: Bearer $token" "$url"
    else
      curl -fsSL "$url"
    fi
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    if [ -n "$token" ]; then
      wget -q --header="Authorization: Bearer $token" -O - "$url"
    else
      wget -q -O - "$url"
    fi
    return
  fi

  die "curl or wget is required."
}

validate_repo() {
  printf '%s\n' "$1" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' ||
    die "Invalid GitHub repository: $1"
}

validate_tag() {
  printf '%s\n' "$1" |
    grep -Eq '^rust-v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta)(\.[0-9]+)?)?-apohl79-[1-9][0-9]*$' ||
    die "Invalid apohl79 fork tag: $1"
}

validate_target() {
  case "$1" in
    aarch64-apple-darwin | x86_64-apple-darwin | aarch64-unknown-linux-musl | x86_64-unknown-linux-musl)
      ;;
    *)
      die "Unsupported target: $1"
      ;;
  esac
}

read_workspace_version() {
  cargo_toml="$repo_root/codex-rs/Cargo.toml"
  [ -f "$cargo_toml" ] || return 1

  awk '
    $0 == "[workspace.package]" {
      in_workspace_package = 1
      next
    }
    in_workspace_package && /^\[/ {
      exit
    }
    in_workspace_package && /^[[:space:]]*version[[:space:]]*=/ {
      version = $0
      sub(/^[^"]*"/, "", version)
      sub(/".*$/, "", version)
      print version
      exit
    }
  ' "$cargo_toml"
}

read_fork_build_number() {
  build_number_file="$repo_root/scripts/apohl79_build_number.txt"
  [ -f "$build_number_file" ] || return 1

  build_number="$(awk 'NR == 1 { print $1; exit }' "$build_number_file")"
  printf '%s\n' "$build_number" | grep -Eq '^[1-9][0-9]*$' || return 1
  printf '%s\n' "$build_number"
}

current_fork_tag() {
  if [ -n "$APOHL79_TAG" ]; then
    validate_tag "$APOHL79_TAG"
    printf '%s\n' "$APOHL79_TAG"
    return
  fi

  tag=""
  if command -v git >/dev/null 2>&1 && [ -d "$repo_root/.git" ]; then
    tag="$(
      git -C "$repo_root" tag --points-at HEAD 2>/dev/null |
        grep -E '^rust-v[0-9].*-apohl79-[1-9][0-9]*$' |
        tail -n 1 || true
    )"
  fi

  if [ -z "$tag" ]; then
    version="$(read_workspace_version || true)"
    build_number="$(read_fork_build_number || true)"
    [ -n "$version" ] && [ -n "$build_number" ] ||
      die "Could not resolve the current fork tag. Pass --tag rust-v<version>-apohl79-<build-number>."
    tag="rust-v$version-apohl79-$build_number"
  fi

  validate_tag "$tag"
  printf '%s\n' "$tag"
}

detect_target() {
  if [ -n "$APOHL79_TARGET" ]; then
    validate_target "$APOHL79_TARGET"
    printf '%s\n' "$APOHL79_TARGET"
    return
  fi

  case "$(uname -s)" in
    Darwin)
      os="darwin"
      ;;
    Linux)
      os="linux"
      ;;
    *)
      die "Only macOS and Linux are supported by this installer."
      ;;
  esac

  case "$(uname -m)" in
    arm64 | aarch64)
      arch="aarch64"
      ;;
    x86_64 | amd64)
      arch="x86_64"
      ;;
    *)
      die "Unsupported architecture: $(uname -m)"
      ;;
  esac

  if [ "$os" = "darwin" ] && [ "$arch" = "x86_64" ]; then
    if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = "1" ]; then
      arch="aarch64"
    fi
  fi

  if [ "$os" = "darwin" ]; then
    printf '%s-apple-darwin\n' "$arch"
  elif [ "$arch" = "aarch64" ]; then
    printf 'aarch64-unknown-linux-musl\n'
  else
    printf 'x86_64-unknown-linux-musl\n'
  fi
}

release_metadata_url() {
  tag="$1"
  printf 'https://api.github.com/repos/%s/releases/tags/%s\n' "$APOHL79_REPO" "$tag"
}

release_url_for_asset() {
  tag="$1"
  asset="$2"
  printf 'https://github.com/%s/releases/download/%s/%s\n' "$APOHL79_REPO" "$tag" "$asset"
}

release_asset_digest() {
  tag="$1"
  asset="$2"
  release_json="$(download_text "$(release_metadata_url "$tag")")"

  digest="$(printf '%s\n' "$release_json" | awk -v asset="$asset" '
    /"name":[[:space:]]*"[^"]+"/ {
      name = $0
      sub(/^.*"name":[[:space:]]*"/, "", name)
      sub(/".*$/, "", name)
      if (name == asset) {
        in_asset = 1
        asset_depth = depth
      }
    }

    in_asset && /"digest":[[:space:]]*"[^"]+"/ {
      digest = $0
      sub(/^.*"digest":[[:space:]]*"/, "", digest)
      sub(/".*$/, "", digest)
    }

    {
      line = $0
      opens = gsub(/\{/, "{", line)
      closes = gsub(/\}/, "}", line)
      depth += opens - closes

      if (in_asset && depth < asset_depth) {
        in_asset = 0
      }
    }

    END {
      if (digest != "") {
        print digest
      }
    }
  ')"

  case "$digest" in
    sha256:????????????????????????????????????????????????????????????????)
      printf '%s\n' "${digest#sha256:}"
      ;;
    *)
      die "Could not find release asset $asset for tag $tag in $APOHL79_REPO."
      ;;
  esac
}

file_sha256() {
  path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
    return
  fi

  if command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$path" | sed 's/^.*= //'
    return
  fi

  die "sha256sum, shasum, or openssl is required to verify the download."
}

verify_archive_digest() {
  archive_path="$1"
  expected_digest="$2"
  actual_digest="$(file_sha256 "$archive_path")"

  if [ "$actual_digest" != "$expected_digest" ]; then
    printf 'Expected: %s\n' "$expected_digest" >&2
    printf 'Actual:   %s\n' "$actual_digest" >&2
    die "Downloaded archive checksum did not match."
  fi
}

replace_path_with_symlink() {
  link_path="$1"
  link_target="$2"
  tmp_link="$3"

  rm -f "$tmp_link"
  ln -s "$link_target" "$tmp_link"

  if mv -Tf "$tmp_link" "$link_path" 2>/dev/null; then
    return
  fi

  if mv -hf "$tmp_link" "$link_path" 2>/dev/null; then
    return
  fi

  rm -f "$link_path"
  mv -f "$tmp_link" "$link_path"
}

release_dir_is_complete() {
  release_dir="$1"
  expected_name="$2"

  [ -d "$release_dir" ] &&
    [ "$(basename "$release_dir")" = "$expected_name" ] &&
    [ -f "$release_dir/codex-package.json" ] &&
    [ -x "$release_dir/bin/codex" ] &&
    [ -x "$release_dir/bin/codex-code-mode-host" ] &&
    [ -x "$release_dir/codex" ] &&
    [ -x "$release_dir/codex-path/rg" ]
}

install_zip_release() {
  release_dir="$1"
  archive_path="$2"
  stage_release="$RELEASES_DIR/.staging.$(basename "$release_dir").$$"

  mkdir -p "$RELEASES_DIR"
  rm -rf "$stage_release"
  mkdir -p "$stage_release"
  unzip -q "$archive_path" -d "$stage_release"

  [ -f "$stage_release/bin/codex" ] || die "Archive is missing bin/codex."
  [ -f "$stage_release/bin/codex-code-mode-host" ] || die "Archive is missing bin/codex-code-mode-host."
  [ -f "$stage_release/codex-path/rg" ] || die "Archive is missing codex-path/rg."
  chmod 0755 \
    "$stage_release/bin/codex" \
    "$stage_release/bin/codex-code-mode-host" \
    "$stage_release/codex-path/rg"
  if [ -f "$stage_release/codex-resources/zsh/bin/zsh" ]; then
    chmod 0755 "$stage_release/codex-resources/zsh/bin/zsh"
  fi
  ln -sf "bin/codex" "$stage_release/codex"

  if [ -e "$release_dir" ] || [ -L "$release_dir" ]; then
    rm -rf "$release_dir"
  fi
  mv "$stage_release" "$release_dir"
}

update_current_link() {
  release_dir="$1"
  tmp_link="$STANDALONE_ROOT/.current.$$"

  mkdir -p "$STANDALONE_ROOT"
  replace_path_with_symlink "$CURRENT_LINK" "$release_dir" "$tmp_link"
}

update_visible_command() {
  mkdir -p "$BIN_DIR"
  tmp_link="$BIN_DIR/.codex.$$"
  tmp_host_link="$BIN_DIR/.codex-code-mode-host.$$"

  replace_path_with_symlink "$BIN_PATH" "$CURRENT_LINK/bin/codex" "$tmp_link"
  replace_path_with_symlink \
    "$HOST_BIN_PATH" \
    "$CURRENT_LINK/bin/codex-code-mode-host" \
    "$tmp_host_link"
}

cleanup() {
  if [ -n "$tmp_dir" ]; then
    rm -rf "$tmp_dir"
  fi
}

print_path_note() {
  case ":$PATH:" in
    *":$BIN_DIR:"*)
      step "$BIN_DIR is already on PATH"
      ;;
    *)
      step "Add $BIN_DIR to PATH, or run: $BIN_PATH"
      ;;
  esac
}

parse_args "$@"
validate_repo "$APOHL79_REPO"

tag="$(current_fork_tag)"
target="$(detect_target)"
fork_version="${tag#rust-v}"
asset="codex-$target-$fork_version.zip"
download_url="$(release_url_for_asset "$tag" "$asset")"
expected_digest="$(release_asset_digest "$tag" "$asset")"
release_name="$fork_version-$target"
release_dir="$RELEASES_DIR/$release_name"

step "Found apohl79 Codex release asset"
printf 'Repository: %s\n' "$APOHL79_REPO"
printf 'Tag:        %s\n' "$tag"
printf 'Target:     %s\n' "$target"
printf 'Asset:      %s\n' "$asset"
printf 'SHA256:     %s\n' "$expected_digest"
printf 'URL:        %s\n' "$download_url"

if [ "$CHECK_ONLY" = true ]; then
  exit 0
fi

require_command mktemp
require_command unzip

tmp_dir="$(mktemp -d)"
trap cleanup EXIT INT TERM

if ! release_dir_is_complete "$release_dir" "$release_name"; then
  archive_path="$tmp_dir/$asset"
  step "Downloading $asset"
  download_file "$download_url" "$archive_path"
  verify_archive_digest "$archive_path" "$expected_digest"

  step "Installing standalone package to $release_dir"
  install_zip_release "$release_dir" "$archive_path"
fi

update_current_link "$release_dir"
update_visible_command
"$BIN_PATH" --version >/dev/null
print_path_note
printf 'apohl79 Codex CLI %s installed successfully.\n' "$fork_version"
