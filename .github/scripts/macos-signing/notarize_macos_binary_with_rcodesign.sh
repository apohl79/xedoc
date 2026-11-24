#!/usr/bin/env bash

# Submits a signed macOS package ZIP or standalone binary to Apple notarization.
# CI uses rcodesign; macOS release builds fall back to native xcrun notarytool.
# Standalone binaries cannot carry a stapled ticket, so they are submitted in a
# ZIP and the successful notarization log is retained.

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: notarize_macos_binary_with_rcodesign.sh (--preflight | --binary PATH | --archive ZIP) [--report-dir PATH] [--max-wait-seconds SECONDS] [--notarytool-keychain-profile NAME]

Options:
  --preflight                   Validate tools and credentials without submitting.
  --binary PATH                 Signed standalone macOS binary to notarize.
  --archive ZIP                 Signed macOS package ZIP to notarize.
  --report-dir PATH             Directory for notarization logs.
  --max-wait-seconds SECONDS    Maximum rcodesign notarization wait time.
  --notarytool-keychain-profile NAME
                                Keychain profile for xcrun notarytool.
EOF
}

binary_path=""
input_archive=""
preflight=false
notarytool_profile_arg=""
report_dir="${RUNNER_TEMP:-/tmp}/macos-binary-notarization-verification"
max_wait_seconds="600"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --preflight)
      preflight=true
      shift
      ;;
    --binary)
      binary_path="${2:-}"
      shift 2
      ;;
    --archive)
      input_archive="${2:-}"
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
    --notarytool-keychain-profile)
      notarytool_profile_arg="${2:-}"
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

if [[ "$preflight" == false ]] && { [[ -z "$binary_path" && -z "$input_archive" ]] || [[ -n "$binary_path" && -n "$input_archive" ]]; }; then
  echo "Pass exactly one of --binary or --archive." >&2
  usage
  exit 2
fi

input_path="${binary_path:-$input_archive}"
if [[ "$preflight" == false && ! -f "$input_path" ]]; then
  echo "Notarization input does not exist: $input_path" >&2
  exit 1
fi
if [[ -n "$input_archive" && "$input_archive" != *.zip ]]; then
  echo "Notarization archive must be a ZIP: $input_archive" >&2
  exit 1
fi

if [[ ! "$max_wait_seconds" =~ ^[0-9]+$ || "$max_wait_seconds" =~ ^0+$ ]]; then
  echo "--max-wait-seconds must be a positive integer." >&2
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

for tool in zip python3 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool was not found on PATH." >&2
    exit 1
  fi
done
retry_script="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../scripts" && pwd)/xedoc_release_retry.py"
if [[ ! -r "$retry_script" ]]; then
  echo "Missing release retry helper: $retry_script" >&2
  exit 1
fi
mkdir -p "$report_dir"
rm -f "$report_dir/notarytool-profile.txt"
notarization_temp_dir="$(mktemp -d)"
trap 'rm -rf "$notarization_temp_dir" >/dev/null' EXIT

notarytool_profile=""
notarytool_profile_candidates=()

add_notarytool_profile_candidate() {
  local candidate="$1"
  local existing

  [[ -n "$candidate" ]] || return 0
  if [[ -n "${notarytool_profile_candidates[*]:-}" ]]; then
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

  local profile_pattern
  profile_pattern='
    function value(line) {
      sub(/^.*<blob>="/, "", line)
      sub(/".*$/, "", line)
      return line
    }
    function emit_profile(line, profile) {
      if (match(line, /com[.]apple[.]gke[.]notary[.]tool[.]saved-creds[.]/)) {
        profile = substr(line, RSTART + RLENGTH)
        sub(/".*$/, "", profile)
        if (profile != "") {
          print profile
        }
      } else if (match(line, /com[.]apple[.]gke[.]notary[.]tool[.]/)) {
        profile = substr(line, RSTART + RLENGTH)
        sub(/".*$/, "", profile)
        if (profile != "" && profile !~ /^saved-creds[.]/) {
          print profile
        }
      }
    }
    function emit() {
      if (account != "" && service == "com.apple.gke.notary.tool") {
        if (account ~ /com[.]apple[.]gke[.]notary[.]tool[.]saved-creds[.]/) {
          emit_profile(account)
        } else {
          print account
        }
      } else if (service ~ /^com[.]apple[.]gke[.]notary[.]tool[.]/) {
        emit_profile(service)
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
    { emit_profile($0) }
    END { emit() }
  '

  local profiles
  profiles="$(security dump-keychain 2>/dev/null | awk "$profile_pattern" || true)"
  if [[ -n "$profiles" ]]; then
    printf '%s\n' "$profiles"
    return 0
  fi

  security find-generic-password \
    -s "com.apple.gke.notary.tool" \
    2>/dev/null | awk "$profile_pattern" || true
}

if [[ "$notarization_backend" == "notarytool" ]]; then
  if [[ -n "$notarytool_profile_arg" ]]; then
    add_notarytool_profile_candidate "$notarytool_profile_arg"
  else
    while IFS= read -r candidate; do
      add_notarytool_profile_candidate "$candidate"
    done < <(discover_notarytool_profiles_from_keychain)
    add_notarytool_profile_candidate "${NOTARYTOOL_PROFILE:-}"
    profile_candidates="${NOTARYTOOL_PROFILE_CANDIDATES:-}"
    for candidate in ${profile_candidates//,/ }; do
      add_notarytool_profile_candidate "$candidate"
    done
    add_notarytool_profile_candidate "app-notary"
    add_notarytool_profile_candidate "xedoc-notary"
    add_notarytool_profile_candidate "xedoc"
    add_notarytool_profile_candidate "notarytool"
  fi

  failed_notarytool_profiles=()
  for candidate in "${notarytool_profile_candidates[@]}"; do
    if python3 "$retry_script" -- xcrun notarytool history \
      --keychain-profile "$candidate" \
      --output-format json \
      --no-progress >"$notarization_temp_dir/history.log" 2>&1; then
      notarytool_profile="$candidate"
      break
    fi
    failed_notarytool_profiles+=("$candidate")
  done
  if [[ -z "$notarytool_profile" ]]; then
    cat "$notarization_temp_dir/history.log" >&2
    printf 'No usable notarytool keychain profile was found (checked: %s).\n' \
      "${failed_notarytool_profiles[*]}" >&2
    exit 1
  fi
fi

if [[ "$notarization_backend" == "rcodesign" ]]; then
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

if [[ "$notarization_backend" == "rcodesign" ]]; then
  api_key_path="$notarization_temp_dir/app-store-connect-api-key.json"
  rcodesign encode-app-store-connect-api-key \
    --output-path "$api_key_path" \
    "$APPLE_NOTARIZATION_ISSUER_ID" \
    "$APPLE_NOTARIZATION_KEY_ID" \
    "$private_key_path" \
    >"$report_dir/encode-app-store-connect-api-key.log" 2>&1
  if rcodesign notary-submit --help >/dev/null 2>&1; then
    rcodesign_submit="notary-submit"
    if rcodesign notary-wait --help >/dev/null 2>&1; then
      rcodesign_wait="notary-wait"
    else
      echo "rcodesign supports notary-submit but not notary-wait." >&2
      exit 1
    fi
  else
    rcodesign notarize --help >/dev/null
    rcodesign_submit="notarize"
    if rcodesign notarize-wait --help >/dev/null 2>&1; then
      rcodesign_wait="notarize-wait"
    else
      echo "rcodesign supports notarize but not notarize-wait." >&2
      exit 1
    fi
  fi
  python3 "$retry_script" -- rcodesign notary-list --api-key-file "$api_key_path" \
    >"$report_dir/credentials-preflight.log" 2>&1
fi
if [[ "$preflight" == true ]]; then
  if [[ "$notarization_backend" == "notarytool" ]]; then
    printf '%s\n' "$notarytool_profile" >"$report_dir/notarytool-profile.txt"
  fi
  echo "Notarization preflight passed ($notarization_backend)."
  exit 0
fi

input_name="$(basename "$input_path")"
archive_path="$input_archive"
if [[ -n "$binary_path" ]]; then
  archive_path="$notarization_temp_dir/${input_name}.zip"
  (
    cd "$(dirname "$binary_path")"
    zip -q "$archive_path" "$input_name"
  )
fi

notarization_log="$report_dir/${input_name}-notarization.log"
case "$notarization_backend" in
  rcodesign)
    if ! rcodesign "$rcodesign_submit" \
      --api-key-file "$api_key_path" \
      "$archive_path" \
      >"$notarization_temp_dir/submission.log" 2>&1; then
      cat "$notarization_temp_dir/submission.log" >&2
      echo "Notarization submission failed; it will not be retried because Apple may have accepted it." >&2
      exit 1
    fi
    cat "$notarization_temp_dir/submission.log" | tee "$notarization_log"
    submission_id="$(
      python3 -c '
import re, sys
text = sys.stdin.read()
matches = re.findall(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b", text)
print(matches[-1] if matches else "")
' <"$notarization_temp_dir/submission.log"
    )"
    if [[ -z "$submission_id" ]]; then
      echo "rcodesign submitted the notarization but did not expose a submission ID; refusing to submit again." >&2
      exit 1
    fi
    python3 "$retry_script" -- rcodesign "$rcodesign_wait" \
      --api-key-file "$api_key_path" \
      --max-wait-seconds "$max_wait_seconds" \
      "$submission_id" \
      2>&1 | tee -a "$notarization_log"
    python3 "$retry_script" -- rcodesign notary-log \
      --api-key-file "$api_key_path" "$submission_id" \
      >"$notarization_temp_dir/submission-result.json"
    cat "$notarization_temp_dir/submission-result.json" | tee -a "$notarization_log"
    python3 -c 'import json, sys; result = json.load(open(sys.argv[1])); sys.exit(0 if result.get("status") == "Accepted" else "Notarization not accepted: " + str(result))' \
      "$notarization_temp_dir/submission-result.json"
    ;;
  notarytool)
    if ! xcrun notarytool submit \
        "$archive_path" \
        --keychain-profile "$notarytool_profile" \
        --no-wait \
        --output-format json \
        >"$report_dir/${input_name}-submission.json"; then
      echo "Notarization submission failed; it will not be retried because Apple may have accepted it." >&2
      exit 1
    fi
    submission_id="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["id"])' \
      "$report_dir/${input_name}-submission.json")"
    python3 "$retry_script" -- xcrun notarytool wait \
        "$submission_id" \
        --keychain-profile "$notarytool_profile" \
        --timeout "${max_wait_seconds}s" \
        --output-format json \
        >"$notarization_log"
    cat "$notarization_log"
    python3 -c 'import json, sys; result = json.load(open(sys.argv[1])); sys.exit(0 if result.get("status") == "Accepted" else "Notarization not accepted: " + str(result))' "$notarization_log"
    ;;
esac

{
  echo "input_name=$input_name"
  echo "max_wait_seconds=$max_wait_seconds"
  echo "input_sha256=$(shasum -a 256 "$input_path" | awk '{ print $1 }')"
  echo "notarization_backend=$notarization_backend"
  echo "notarytool_profile=$notarytool_profile"
  echo "notarization=completed"
} >"$report_dir/${input_name}-notarization-summary.txt"
