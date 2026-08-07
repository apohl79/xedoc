#!/bin/sh

set -eu

APOHL79_REPO="${CODEX_APOHL79_REPO:-apohl79/codex}"
APOHL79_TAG="${CODEX_APOHL79_TAG:-}"
APOHL79_TARGET="${CODEX_APOHL79_TARGET:-}"
LOCAL_ZIP="${CODEX_APOHL79_LOCAL_ZIP:-}"
BIN_DIR="${CODEX_INSTALL_DIR:-$HOME/.local/bin}"
BIN_PATH="$BIN_DIR/codex"
HOST_BIN_PATH="$BIN_DIR/codex-code-mode-host"
SESSION_CONTROL_BIN_PATH="$BIN_DIR/codex-session"
CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
NON_INTERACTIVE="${CODEX_NON_INTERACTIVE:-false}"
ZSHRC_PATH="$HOME/.zshrc"
ZSHRC_APP_SERVER_CHOICE_PATH="$CODEX_HOME_DIR/app-server-daemon/zshrc-start"
CODEX_PROVIDERS_INSTALL_CHOICE_PATH="$CODEX_HOME_DIR/codex-providers/install"
CODEX_PROVIDERS_INSTALL_URL="https://raw.githubusercontent.com/apohl79/codex-providers/main/install.sh"
STANDALONE_ROOT="$CODEX_HOME_DIR/packages/standalone"
RELEASES_DIR="$STANDALONE_ROOT/releases"
CURRENT_LINK="$STANDALONE_ROOT/current"
CHECK_ONLY=false
tmp_dir=""
codex_providers_action="skipped"
app_server_was_running=false

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
Usage: install-apohl79.sh [--tag TAG] [--target TARGET] [--repo OWNER/REPO] [--local-zip PATH] [--check]

Downloads and installs the apohl79 Codex fork binary release for the current fork tag,
or installs a local release ZIP without contacting GitHub.

Options:
  --tag TAG        Fork release tag to install. Defaults to the current rust-v*-apohl79-N tag.
  --target TARGET  Release target triple. Defaults to the current platform.
  --repo OWNER/REPO
                   GitHub repository to read releases from. Defaults to apohl79/codex.
  --local-zip PATH  Install a local release ZIP instead of downloading one from GitHub.
                   The ZIP must contain codex-package.json.
  --check          Verify that the release asset exists, then print the plan and exit.
  -h, --help       Show this help.

Environment:
  CODEX_APOHL79_TAG     Same as --tag.
  CODEX_APOHL79_TARGET  Same as --target.
  CODEX_APOHL79_REPO    Same as --repo.
  CODEX_APOHL79_LOCAL_ZIP
                       Same as --local-zip.
  CODEX_INSTALL_DIR     Directory for the visible codex symlinks. Defaults to ~/.local/bin.
  CODEX_HOME            Codex home directory. Defaults to ~/.codex.
  CODEX_NON_INTERACTIVE  Set to 1, true, or yes to skip prompts.
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
      --local-zip)
        [ "$#" -ge 2 ] || die "--local-zip requires a value."
        LOCAL_ZIP="$2"
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

prompt_user_available() {
  case "$NON_INTERACTIVE" in
    1 | [Tt][Rr][Uu][Ee] | [Yy][Ee][Ss])
      return 1
      ;;
  esac

  if ( : </dev/tty ) 2>/dev/null || [ -t 0 ]; then
    return 0
  fi

  return 1
}

prompt_yes_no() {
  prompt="$1"
  if ! prompt_user_available; then
    return 1
  fi

  printf '%s [y/N] ' "$prompt" >/dev/tty 2>/dev/null ||
    printf '%s [y/N] ' "$prompt"
  if ( : </dev/tty ) 2>/dev/null; then
    IFS= read -r answer </dev/tty || return 1
  else
    IFS= read -r answer || return 1
  fi

  case "$answer" in
    y | Y | yes | YES)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

app_server_is_running() {
  [ -x "$BIN_PATH" ] &&
    "$BIN_PATH" app-server daemon version >/dev/null 2>&1
}

restart_running_app_server() {
  if [ "$app_server_was_running" != true ]; then
    return 0
  fi

  if ! prompt_user_available; then
    step "App-server was running before the upgrade; leaving it running in non-interactive mode"
    return
  fi

  if ! prompt_yes_no "The Codex app-server is running. Restart it to use the upgraded version?"; then
    step "Leaving the running Codex app-server unchanged"
    return
  fi

  step "Restarting Codex app-server"
  if "$BIN_PATH" app-server daemon restart >/dev/null 2>&1; then
    step "Codex app-server restarted"
  else
    printf 'WARNING: Could not restart the running Codex app-server. Run: "%s" app-server daemon restart\n' \
      "$BIN_PATH" >&2
  fi
}

read_zshrc_app_server_choice() {
  if [ ! -f "$ZSHRC_APP_SERVER_CHOICE_PATH" ]; then
    return 1
  fi

  choice="$(sed -n '1p' "$ZSHRC_APP_SERVER_CHOICE_PATH" 2>/dev/null || true)"
  case "$choice" in
    enabled | disabled)
      printf '%s\n' "$choice"
      return 0
      ;;
  esac

  return 1
}

write_zshrc_app_server_choice() {
  mkdir -p "$(dirname "$ZSHRC_APP_SERVER_CHOICE_PATH")"
  printf '%s\n' "$1" >"$ZSHRC_APP_SERVER_CHOICE_PATH"
}

rewrite_zshrc_app_server_block() {
  zshrc_begin_marker="# >>> Codex app-server installer >>>"
  zshrc_end_marker="# <<< Codex app-server installer <<<"
  zshrc_start_line="  \"$BIN_PATH\" app-server daemon start >/dev/null 2>&1 &!"
  tmp_profile="$tmp_dir/zshrc.$$.tmp"

  awk \
    -v begin="$zshrc_begin_marker" \
    -v end="$zshrc_end_marker" \
    -v bin="$BIN_PATH" \
    -v start_line="$zshrc_start_line" '
      BEGIN {
        in_block = 0
        replaced = 0
      }
      $0 == begin {
        if (!replaced) {
          print begin
          print "if [ -x \"" bin "\" ]; then"
          print start_line
          print "fi"
          print end
          replaced = 1
        }
        in_block = 1
        next
      }
      in_block {
        if ($0 == end) {
          in_block = 0
        }
        next
      }
      {
        print
      }
      END {
        if (in_block != 0) {
          exit 1
        }
      }
    ' "$ZSHRC_PATH" >"$tmp_profile"
  mv "$tmp_profile" "$ZSHRC_PATH"
}

append_zshrc_app_server_block() {
  zshrc_begin_marker="# >>> Codex app-server installer >>>"
  zshrc_end_marker="# <<< Codex app-server installer <<<"

  {
    printf '\n%s\n' "$zshrc_begin_marker"
    printf 'if [ -x "%s" ]; then\n' "$BIN_PATH"
    printf '  "%s" app-server daemon start >/dev/null 2>&1 &!\n' "$BIN_PATH"
    printf 'fi\n%s\n' "$zshrc_end_marker"
  } >>"$ZSHRC_PATH"
}

configure_zshrc_app_server() {
  zshrc_app_server_action="skipped"
  choice="$(read_zshrc_app_server_choice || true)"

  case "$choice" in
    enabled | disabled)
      ;;
    *)
      if ! prompt_user_available; then
        return
      fi
      if prompt_yes_no "Start the Codex app-server automatically from ~/.zshrc?"; then
        choice="enabled"
      else
        choice="disabled"
      fi
      write_zshrc_app_server_choice "$choice"
      ;;
  esac

  if [ "$choice" = "disabled" ]; then
    zshrc_app_server_action="disabled"
    return
  fi

  zshrc_begin_marker="# >>> Codex app-server installer >>>"
  zshrc_end_marker="# <<< Codex app-server installer <<<"
  zshrc_start_line="  \"$BIN_PATH\" app-server daemon start >/dev/null 2>&1 &!"
  if [ -f "$ZSHRC_PATH" ] &&
    grep -F "$zshrc_begin_marker" "$ZSHRC_PATH" >/dev/null 2>&1 &&
    grep -F "$zshrc_end_marker" "$ZSHRC_PATH" >/dev/null 2>&1 &&
    grep -F "$zshrc_start_line" "$ZSHRC_PATH" >/dev/null 2>&1; then
    zshrc_app_server_action="configured"
    return
  fi

  if [ -f "$ZSHRC_PATH" ] &&
    grep -F "$zshrc_begin_marker" "$ZSHRC_PATH" >/dev/null 2>&1; then
    rewrite_zshrc_app_server_block
    zshrc_app_server_action="updated"
    return
  fi

  append_zshrc_app_server_block
  zshrc_app_server_action="added"
}

codex_providers_is_installed() {
  command -v codex-providers >/dev/null 2>&1 ||
    [ -x "$HOME/bin/codex-providers" ] ||
    [ -x "$HOME/.local/bin/codex-providers" ]
}

configure_codex_providers() {
  if codex_providers_is_installed; then
    codex_providers_action="already-installed"
    return
  fi

  choice=""
  if [ -f "$CODEX_PROVIDERS_INSTALL_CHOICE_PATH" ]; then
    choice="$(sed -n '1p' "$CODEX_PROVIDERS_INSTALL_CHOICE_PATH" 2>/dev/null || true)"
  fi
  if [ "$choice" = "disabled" ]; then
    codex_providers_action="disabled"
    return
  fi

  if ! prompt_user_available; then
    return
  fi

  if prompt_yes_no "Install optional codex-providers for Claude, DeepSeek, and Gemini support?"; then
    require_command curl
    require_command bash
    step "Installing codex-providers"
    if ! bash -o pipefail -c 'curl -fsSL "$1" | bash' codex-providers-installer "$CODEX_PROVIDERS_INSTALL_URL"; then
      die "codex-providers installation failed."
    fi
    if ! codex_providers_is_installed; then
      die "codex-providers installer completed without installing the codex-providers command."
    fi
    codex_providers_action="installed"
    return
  fi

  mkdir -p "$(dirname "$CODEX_PROVIDERS_INSTALL_CHOICE_PATH")"
  printf '%s\n' "disabled" >"$CODEX_PROVIDERS_INSTALL_CHOICE_PATH"
  codex_providers_action="disabled"
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
    if [ -n "$version" ] && [ -n "$build_number" ]; then
      tag="rust-v$version-apohl79-$build_number"
    else
      tag="$(latest_fork_tag)"
    fi
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

latest_release_metadata_url() {
  printf 'https://api.github.com/repos/%s/releases/latest\n' "$APOHL79_REPO"
}

latest_fork_tag() {
  if ! release_json="$(download_text "$(latest_release_metadata_url)")"; then
    die "Could not fetch the latest apohl79 Codex release metadata. GitHub API may be unavailable or rate limited."
  fi

  tag="$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || die "Could not resolve the latest apohl79 Codex release tag."
  validate_tag "$tag"
  printf '%s\n' "$tag"
}

local_package_metadata_field() {
  field="$1"
  printf '%s\n' "$local_package_metadata" |
    sed -n "s/.*\"$field\":[[:space:]]*\"\([^\"]*\)\".*/\1/p" |
    head -n 1
}

prepare_local_package() {
  [ -n "$LOCAL_ZIP" ] || return 1
  [ -f "$LOCAL_ZIP" ] || die "Local package ZIP does not exist: $LOCAL_ZIP"
  case "$LOCAL_ZIP" in
    *.zip) ;;
    *) die "Local package must be a ZIP file: $LOCAL_ZIP" ;;
  esac

  LOCAL_ZIP="$(CDPATH='' cd "$(dirname "$LOCAL_ZIP")" && pwd)/$(basename "$LOCAL_ZIP")"
  local_package_metadata="$(unzip -p "$LOCAL_ZIP" codex-package.json 2>/dev/null || true)"
  [ -n "$local_package_metadata" ] ||
    die "Local package ZIP is missing codex-package.json: $LOCAL_ZIP"

  local_version="$(local_package_metadata_field version)"
  local_target="$(local_package_metadata_field target)"
  [ -n "$local_version" ] ||
    die "Local package metadata is missing version: $LOCAL_ZIP"
  [ -n "$local_target" ] ||
    die "Local package metadata is missing target: $LOCAL_ZIP"

  local_tag="rust-v$local_version"
  validate_tag "$local_tag"
  if [ -n "$APOHL79_TAG" ]; then
    validate_tag "$APOHL79_TAG"
    [ "$APOHL79_TAG" = "$local_tag" ] ||
      die "Local package version does not match tag $APOHL79_TAG."
  else
    APOHL79_TAG="$local_tag"
  fi

  validate_target "$local_target"
  if [ -n "$APOHL79_TARGET" ]; then
    validate_target "$APOHL79_TARGET"
    [ "$APOHL79_TARGET" = "$local_target" ] ||
      die "Local package target does not match target $APOHL79_TARGET."
  else
    APOHL79_TARGET="$local_target"
  fi
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
    [ -x "$release_dir/bin/codex-session" ] &&
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
  [ -f "$stage_release/bin/codex-session" ] || die "Archive is missing bin/codex-session."
  [ -f "$stage_release/codex-path/rg" ] || die "Archive is missing codex-path/rg."
  chmod 0755 \
    "$stage_release/bin/codex" \
    "$stage_release/bin/codex-code-mode-host" \
    "$stage_release/bin/codex-session" \
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
  tmp_session_link="$BIN_DIR/.codex-session.$$"

  replace_path_with_symlink "$BIN_PATH" "$CURRENT_LINK/bin/codex" "$tmp_link"
  replace_path_with_symlink \
    "$SESSION_CONTROL_BIN_PATH" \
    "$CURRENT_LINK/bin/codex-session" \
    "$tmp_session_link"
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

print_zshrc_app_server_instructions() {
  case "$zshrc_app_server_action" in
    added)
      step "App-server startup was added to $ZSHRC_PATH"
      ;;
    updated)
      step "App-server startup was updated in $ZSHRC_PATH"
      ;;
    configured)
      step "App-server startup is already configured in $ZSHRC_PATH"
      ;;
    disabled)
      step "App-server startup remains disabled by your saved installer choice"
      ;;
  esac
}

print_codex_providers_instructions() {
  case "$codex_providers_action" in
    installed)
      step "codex-providers installed. Run: codex-providers setup"
      ;;
    disabled)
      step "codex-providers installation remains disabled by your saved installer choice"
      ;;
  esac
}

parse_args "$@"
validate_repo "$APOHL79_REPO"

if [ -n "$LOCAL_ZIP" ]; then
  require_command unzip
  prepare_local_package
fi

tag="$(current_fork_tag)"
target="$(detect_target)"
fork_version="${tag#rust-v}"
asset="codex-$target-$fork_version.zip"
release_name="$fork_version-$target"
release_dir="$RELEASES_DIR/$release_name"

if [ -n "$LOCAL_ZIP" ]; then
  local_digest="$(file_sha256 "$LOCAL_ZIP")"
  step "Found local apohl79 Codex package"
  printf 'Package:    %s\n' "$LOCAL_ZIP"
  printf 'SHA256:     %s\n' "$local_digest"
else
  download_url="$(release_url_for_asset "$tag" "$asset")"
  expected_digest="$(release_asset_digest "$tag" "$asset")"
  step "Found apohl79 Codex release asset"
  printf 'Repository: %s\n' "$APOHL79_REPO"
  printf 'Asset:      %s\n' "$asset"
  printf 'SHA256:     %s\n' "$expected_digest"
  printf 'URL:        %s\n' "$download_url"
fi
printf 'Tag:        %s\n' "$tag"
printf 'Target:     %s\n' "$target"

if [ "$CHECK_ONLY" = true ]; then
  exit 0
fi

if app_server_is_running; then
  app_server_was_running=true
fi

require_command mktemp
require_command unzip

tmp_dir="$(mktemp -d)"
trap cleanup EXIT INT TERM

if ! release_dir_is_complete "$release_dir" "$release_name"; then
  if [ -n "$LOCAL_ZIP" ]; then
    archive_path="$LOCAL_ZIP"
    step "Installing local package ZIP"
  else
    archive_path="$tmp_dir/$asset"
    step "Downloading $asset"
    download_file "$download_url" "$archive_path"
    verify_archive_digest "$archive_path" "$expected_digest"
  fi

  step "Installing standalone package to $release_dir"
  install_zip_release "$release_dir" "$archive_path"
fi

update_current_link "$release_dir"
update_visible_command
"$BIN_PATH" --version >/dev/null
restart_running_app_server
configure_zshrc_app_server
configure_codex_providers

# Deploy statusline script
STATUSLINE_DST="$CODEX_HOME_DIR/statusline.sh"
if [ -f "$STATUSLINE_DST" ]; then
  step "Keeping existing statusline script at $STATUSLINE_DST"
else
  STATUSLINE_SRC="$repo_root/scripts/statusline.sh"
  if [ ! -f "$STATUSLINE_SRC" ] && [ -z "$LOCAL_ZIP" ]; then
    STATUSLINE_SRC="$tmp_dir/statusline.sh"
    STATUSLINE_URL="https://raw.githubusercontent.com/$APOHL79_REPO/$tag/scripts/statusline.sh"
    step "Downloading statusline script"
    download_file "$STATUSLINE_URL" "$STATUSLINE_SRC"
  fi
  if [ -f "$STATUSLINE_SRC" ]; then
    step "Deploying statusline script to $STATUSLINE_DST"
    mkdir -p "$CODEX_HOME_DIR"
    cp "$STATUSLINE_SRC" "$STATUSLINE_DST"
    chmod +x "$STATUSLINE_DST"
  elif [ -n "$LOCAL_ZIP" ]; then
    step "Skipping statusline script for local package"
  fi
fi

print_path_note
print_zshrc_app_server_instructions
print_codex_providers_instructions
printf 'apohl79 Codex CLI %s installed successfully.\n' "$fork_version"
