#!/usr/bin/env bash

# Submits a signed standalone macOS binary to Apple notarization. CI uses
# rcodesign; macOS release builds fall back to native xcrun notarytool.
# Standalone binaries cannot carry a stapled ticket, so the binary is submitted
# in a ZIP and the successful notarization log is retained.

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: notarize_macos_binary_with_rcodesign.sh --binary PATH [--report-dir PATH] [--max-wait-seconds SECONDS]

Options:
  --binary PATH                 Signed standalone macOS binary to notarize.
  --report-dir PATH             Directory for notarization logs.
  --max-wait-seconds SECONDS    Maximum rcodesign notarization wait time.
EOF
}

binary_path=""
report_dir="${RUNNER_TEMP:-/tmp}/macos-binary-notarization-verification"
max_wait_seconds="600"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      binary_path="${2:-}"
      shift 2
      ;;
    --report-dir)
      report_dir="${2:-}"
      shift 2
      ;;
    --max-wait-seconds)
      max_wait_seconds="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown notarization argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$binary_path" ]]; then
  echo "--binary is required." >&2
  usage
  exit 2
fi

if [[ ! -f "$binary_path" ]]; then
  echo "Binary does not exist: $binary_path" >&2
  exit 1
fi

if [[ ! "$max_wait_seconds" =~ ^[0-9]+$ ]]; then
  echo "--max-wait-seconds must be a non-negative integer." >&2
  exit 2
fi

if command -v rcodesign >/dev/null 2>&1; then
  notarization_backend="rcodesign"
elif command -v xcrun >/dev/null 2>&1 && xcrun notarytool --version >/dev/null 2>&1; then
  notarization_backend="notarytool"
else
  echo "Neither rcodesign nor xcrun notarytool was found on PATH." >&2
  exit 1
fi

if ! command -v zip >/dev/null 2>&1; then
  echo "zip was not found on PATH." >&2
  exit 1
fi

notarytool_profile=""
notarytool_profile_candidates=()

add_notarytool_profile_candidate() {
  local candidate="$1"
  local existing

  [[ -n "$candidate" ]] || return 0
  if [[ "${#notarytool_profile_candidates[@]}" -gt 0 ]]; then
    for existing in "${notarytool_profile_candidates[@]}"; do
      [[ "$existing" != "$candidate" ]] || return 0
    done
  fi
  notarytool_profile_candidates+=("$candidate")
}

discover_notarytool_profiles_from_keychain() {
  if ! command -v security >/dev/null 2>&1; then
    return 0
  fi

  security dump-keychain 2>/dev/null | awk '
    function value(line) {
      sub(/^.*<blob>="/, "", line)
      sub(/".*$/, "", line)
      return line
    }
    function emit() {
      if (account != "" && service == "com.apple.gke.notary.tool") {
        print account
      } else if (service ~ /^com[.]apple[.]gke[.]notary[.]tool[.]/) {
        sub(/^com[.]apple[.]gke[.]notary[.]tool[.]/, "", service)
        if (service != "") {
          print service
        }
      }
    }
    /^keychain:/ {
      emit()
      account = ""
      service = ""
      next
    }
    /"acct"<blob>=/ { account = value($0) }
    /"svce"<blob>=/ { service = value($0) }
    END { emit() }
  '
}

if [[ "$notarization_backend" == "notarytool" ]]; then
  add_notarytool_profile_candidate "${NOTARYTOOL_PROFILE:-}"
  for candidate in ${NOTARYTOOL_PROFILE_CANDIDATES//,/ }; do
    add_notarytool_profile_candidate "$candidate"
  done
  while IFS= read -r candidate; do
    add_notarytool_profile_candidate "$candidate"
  done < <(discover_notarytool_profiles_from_keychain)
  add_notarytool_profile_candidate "xedoc-notary"
  add_notarytool_profile_candidate "xedoc"
  add_notarytool_profile_candidate "notarytool"

  for candidate in "${notarytool_profile_candidates[@]}"; do
    if xcrun notarytool history \
      --keychain-profile "$candidate" \
      --output-format json \
      --no-progress >/dev/null 2>&1; then
      notarytool_profile="$candidate"
      break
    fi
  done
fi

if [[ -z "$notarytool_profile" ]]; then
  missing_environment=0
  for variable_name in \
    APPLE_NOTARIZATION_ISSUER_ID \
    APPLE_NOTARIZATION_KEY_ID \
    APPLE_NOTARIZATION_KEY_P8
  do
    if [[ -z "${!variable_name:-}" ]]; then
      missing_environment=1
    fi
  done
  if [[ "$missing_environment" -ne 0 ]]; then
    echo "No usable notarytool keychain profile was found, and App Store Connect API-key credentials are not set." >&2
    exit 2
  fi
fi

mkdir -p "$report_dir"

notarization_temp_dir="$(mktemp -d)"
trap 'rm -rf "$notarization_temp_dir" >/dev/null' EXIT

private_key_path=""
if [[ -z "$notarytool_profile" ]]; then
  private_key_path="$notarization_temp_dir/AuthKey_${APPLE_NOTARIZATION_KEY_ID}.p8"
  if ! printf '%s' "$APPLE_NOTARIZATION_KEY_P8" | base64 --decode >"$private_key_path" 2>/dev/null; then
    if ! printf '%s' "$APPLE_NOTARIZATION_KEY_P8" | base64 -D >"$private_key_path" 2>/dev/null; then
      echo "APPLE_NOTARIZATION_KEY_P8 must be a base64-encoded .p8 private key." >&2
      exit 2
    fi
  fi
  chmod 600 "$private_key_path"
fi

binary_name="$(basename "$binary_path")"
archive_path="$notarization_temp_dir/${binary_name}.zip"
(
  cd "$(dirname "$binary_path")"
  zip -q "$archive_path" "$binary_name"
)

notarization_log="$report_dir/${binary_name}-notarization.log"
case "$notarization_backend" in
  rcodesign)
    api_key_path="$notarization_temp_dir/app-store-connect-api-key.json"
    rcodesign encode-app-store-connect-api-key \
      --output-path "$api_key_path" \
      "$APPLE_NOTARIZATION_ISSUER_ID" \
      "$APPLE_NOTARIZATION_KEY_ID" \
      "$private_key_path" \
      >"$report_dir/encode-app-store-connect-api-key.log" 2>&1
    rcodesign notarize \
      --api-key-file "$api_key_path" \
      --max-wait-seconds "$max_wait_seconds" \
      --wait \
      "$archive_path" \
      2>&1 | tee "$notarization_log"
    ;;
  notarytool)
    if [[ -n "$notarytool_profile" ]]; then
      xcrun notarytool submit \
        "$archive_path" \
        --keychain-profile "$notarytool_profile" \
        --wait \
        --timeout "${max_wait_seconds}s" \
        --output-format json \
        2>&1 | tee "$notarization_log"
    else
      xcrun notarytool submit \
        "$archive_path" \
        --key "$private_key_path" \
        --key-id "$APPLE_NOTARIZATION_KEY_ID" \
        --issuer "$APPLE_NOTARIZATION_ISSUER_ID" \
        --wait \
        --timeout "${max_wait_seconds}s" \
        --output-format json \
        2>&1 | tee "$notarization_log"
    fi
    ;;
esac

{
  echo "binary_name=$binary_name"
  echo "max_wait_seconds=$max_wait_seconds"
  echo "binary_sha256=$(shasum -a 256 "$binary_path" | awk '{ print $1 }')"
  echo "notarization_backend=$notarization_backend"
  echo "notarytool_profile=$notarytool_profile"
  echo "notarization=completed"
} >"$report_dir/${binary_name}-notarization-summary.txt"
