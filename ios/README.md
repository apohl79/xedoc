# Xedoc for iPhone and iPad

Native host discovery and a WebKit shell for the existing Xedoc host GUI.
Requires Xcode and XcodeGen (`xcodegen`).

```sh
cd ios
xcodegen generate
open Xedoc.xcodeproj
```

Select an iPhone or iPad and run the `Xedoc` scheme. The host selector scans on
launch and when returning to it. Refresh rescans the current network. A host
can also be entered by its HTTPS origin.

The sidebar starts hidden on iPhone in either orientation and on iPad in
portrait. It docks automatically on iPad in landscape. Swipe right to open it
and swipe left to hide the portrait overlay.
TUI dialogs show arrow, Enter, Escape, and keyboard buttons. Use the keyboard
button to enter text into a TUI menu.

## Notifications

Connecting to a host requests notification permission. Allow it to receive
alerts when an observed turn finishes or starts waiting for input or approval.
Alerts include the host and project name, not conversation content. Change
permission later in iOS Settings → Notifications → Xedoc.

Configured hosts send APNs alerts even while iOS suspends the app. The selected
host subscription remains active in the background, until disconnect, sign-out,
or expiry of the host login. Hosts with no eligible device subscriptions do not
contact APNs. Opening a session does not alert for its existing state.

The sidebar host-name button returns to native host discovery. Switching hosts
waits for the old host to confirm subscription removal; if it is unreachable,
the switch stays blocked. Reconnect that host and retry. Notifications already
accepted by Apple may arrive after removal. Unconfigured or older hosts retain
open-app-only local alerts, with a notice explaining the limitation.

### Host APNs credentials

Enable Push Notifications for the app's explicit Apple App ID. Use separate
topic-specific production and sandbox keys, and refresh provisioning profiles.
The app reads its signed profile's APNs environment; Ad Hoc exports must use
`production`. No signing key belongs in the mobile app or release archive.

The desktop installer checks local credentials first. If they are ready, it
never invokes 1Password. Otherwise it reuses an authenticated `OP_SESSION` or
establishes one user-approved in-memory session with `op signin`, then retrieves
only missing keys from `Private/Xedoc APNs Production` and
`Private/Xedoc APNs Sandbox`. The existing 1Password CLI and access to that
vault are required for retrieval. For manual `apns setup`, establish the
session in the same terminal with `eval "$(op signin)"` first.
No APNs configuration file, team/key IDs, or separate setup command is required.
An unavailable sign-in is reported explicitly; it does not enable background
push or prevent using the desktop app.

Runtime discovers the standard Keychain entries on macOS and owner-private
`$XEDOC_HOME/host-gui/apns-KEY_ID.p8` files on Linux. Existing `apns.json`
overrides remain supported and are never replaced. Defaults match the signed
app: team `62QZB3NCZR`, topic `org.e47.xedoc.mobile`, production key
`8277F92N2F`, and sandbox key `G68J93J939`. Other Apple teams still need their
own app signing and credential overrides.

For diagnostics, `xedoc-remote-agentd apns check` validates local credentials
without contacting 1Password or Apple. `apns setup` retries missing-key
provisioning only with an authenticated `OP_SESSION`. Keys are
validated as P-256, never passed in process arguments, and never overwritten.
The standalone build-host checker remains available for signing prerequisites;
a build-only Mac does not need APNs sending credentials.

Host TLS, request, and APNs failures are recorded in
`$XEDOC_HOME/remote-agent/broker-diagnostics.log` (`host-gui-diagnostics.log`
when running the GUI server separately). These owner-private logs rotate at
1 MiB with two backups and omit request payloads, tokens, and private keys.

APNs uses outbound HTTPS/HTTP2; no public inbound host endpoint is required.
Keep keys in the private vault and sending host's Keychain/private credential
files. Never commit them, include them in release artifacts, or copy them into
the iOS application.

## Signing and host trust

Automatic signing defaults to the repository owner's team. Override
`DEVELOPMENT_TEAM` in Xcode or on the `xcodebuild` command line if needed.
Discovery browses `_xedoc-gui._tcp` Bonjour announcements from updated hosts.
It does not require Apple's restricted multicast entitlement. Allow Local
Network access when prompted. Older hosts can be opened by their HTTPS origin.
Simulator discovery does not prove physical-device Local Network permission.

WebKit uses normal system TLS validation, including for session WebSockets.
On a certificate error, choose **Set Up Certificate**, then **Open Certificate
Setup in Safari** to access the host's `/trust` page and Apple profile download.
Verify the CA fingerprint with the administrator through a separate trusted
channel before installing it. The setup sheet explains profile installation
and enabling full trust in Settings; return to it and choose **Retry**
afterward. If Safari cannot reach the page, obtain the public CA profile and
fingerprint directly from the administrator. Discovery never grants trust to
certificates.

For personal devices, use development signing and a profile covering the device:

```sh
xcodebuild -project Xedoc.xcodeproj -scheme Xedoc \
  -destination 'generic/platform=iOS' -archivePath build/Xedoc.xcarchive \
  -allowProvisioningUpdates archive
```

Development builds do not use macOS notarization. iOS alternative-distribution
notarization is a separate App Store Connect review, not `notarytool`.

## Releases and cable installation

### Prepare another release Mac

1. Install full Xcode (not just Command Line Tools), launch it once, accept its
   license, and install iOS platform support. Select it:

   ```sh
   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
   sudo xcodebuild -runFirstLaunch
   ```

2. Install Homebrew using its official installer if needed, then install the
   release tools:

   ```sh
   brew install python@3.12 xcodegen bazelisk uv gh git rustup
   export PATH="$(brew --prefix python@3.12)/libexec/bin:$PATH"
   python3 --version
   ```

   Keep Python 3.10+ on `PATH` in this release terminal. The
   [Homebrew Python formula](https://formulae.brew.sh/formula/python@3.12)
   places its unversioned `python3` link in `libexec/bin`.
   The macOS system `python3` may be too old; `uv` fallback in the release
   entry point does not fix other helpers using `#!/usr/bin/env python3`.

   Clone `apohl79/xedoc` and run subsequent commands from its root. Bazelisk
   selects the repository's pinned Bazel version. For the four-target release,
   configure access to the existing BuildBuddy remote executors using
   `BUILDBUDDY_API_KEY` in the release terminal; retrieve the key through the
   authorized reusable 1Password session, not a literal in shell history.
   Remote execution must support the macOS and Linux target toolchains.
   This credential is separate from Apple's keys. Sign in using `gh auth login`
   with an account allowed to publish releases to `apohl79/xedoc`; the release
   wrapper defaults to the stored `apohl79` account (`--github-account NAME`
   overrides it).

3. In Xcode Settings → Accounts, sign in to the Apple Developer account and
   select team `62QZB3NCZR`. Use **Manage Certificates** to create this Mac's
   **Apple Development**, **Apple Distribution**, and **Developer ID Application**
   identities as permitted by the account. Each identity must have its private
   key in the local login Keychain. A certificate alone is insufficient.
   Prefer Xcode-managed identities; do not export certificates or regenerate
   the existing APNs keys merely to prepare another build host.
   Confirm the explicit App ID `org.e47.xedoc.mobile` has Push Notifications
   enabled. Xcode must refresh its development and Ad Hoc profiles after
   enabling Push. Automatic signing needs account permission to manage profiles
   and devices. Register intended devices or use the cable installer's
   reprovisioning flow below.

4. For desktop notarization, create an app-specific password in the Apple
   account, save it in 1Password, then store a local notarytool profile:

   ```sh
   xcrun notarytool store-credentials xedoc-notary
   ```

   Answer its interactive Apple ID, team, and password prompts; never put the
   password in command arguments or shell history. This stores credentials in
   Keychain. Verify the profile and local signing prerequisites:

   ```sh
   python3 scripts/setup_apple_build_host.py --check --notary-profile xedoc-notary
   # Build-only iOS Mac: no desktop identity or notarization required.
   python3 scripts/setup_apple_build_host.py --check --ios-only
   ```

   Checks verify the selected iOS SDK and valid identities for the selected
   team, but cannot prove portal provisioning permissions or device quotas.
   `--notary-profile` authenticates with Apple; without it notarization
   credentials are explicitly reported as unchecked. Apple signing/timestamp
   and notarization services must be reachable during release creation.

5. If this Mac also serves the host GUI, import the existing APNs keys and run
   the opt-in setup under **Host APNs credentials** above. This does not enable
   Push in Apple's portal, register devices, change provisioning profiles, or
   replace signing identities.

### Build and publish

From a clean, committed `main` checkout with the intended release version:

```sh
scripts/build_xedoc_release.sh \
  --target macos-arm64 --target macos-x86_64 \
  --target linux-arm64 --target linux-x86_64 \
  --notarytool-keychain-profile xedoc-notary
```

On macOS this builds and uploads all four desktop targets **and** the signed
iOS archive. The desktop packages are notarized; the iOS archive uses Ad Hoc
distribution signing with production APNs, not macOS notarization. iOS
alternative-distribution notarization requires a separate App Store Connect
workflow and is not performed by this script.

The macOS release script builds a signed `xedoc-ios-VERSION.zip` alongside the
desktop packages and uploads it to the same GitHub release. Use `--skip-ios` to
omit it, or build only the iOS artifact:

```sh
python3 scripts/build_xedoc_release.py --ios-only
```

The release Mac needs Xcode, XcodeGen, and access to the configured signing team.
The script archives the app, then exports it using Xcode's `release-testing`
(Ad Hoc) distribution method. Automatic signing needs access to an Apple
Distribution certificate and an Ad Hoc provisioning profile; Xcode can create
these when the signed-in account has permission. It never falls back to a
development-signed release. The app version follows the release version.
These are distribution-signed apps, not App Store or notarized builds.
Devices must be registered with the signing team and included in the embedded,
unexpired provisioning profile. The ZIP includes a small Xcode project for new
devices. If the selected device is missing from the profile, the installer uses
Xcode automatic signing to register it with the app's original signing team,
build the bundled source for that device, then archive and export a fresh
Ad Hoc build before installing.
Sign in to that team in Xcode Settings → Accounts first. No XcodeGen is needed
on the installing Mac. The installer never changes signing teams or bypasses
Apple's account permissions, device quotas, or registration processing delays.

On macOS, the CLI installer offers optional iPhone/iPad installation. To install
only the mobile app without changing the desktop installation:

```sh
sh scripts/install/install.sh --ios-only
# Or use a previously downloaded/built artifact:
sh scripts/install/install.sh --ios-only --local-ios-zip /path/to/xedoc-ios-VERSION.zip
```

`--ios-only` defaults to the latest published release, including from a checkout
with an unreleased version. Use `--tag VERSION` to select a specific release.

The installer verifies the GitHub download's SHA-256 digest before extracting it.
With full Xcode and Python 3 installed, accept the prompt, connect and unlock a
device by USB cable, trust the Mac, and enable Developer Mode when required.
If Developer Mode is disabled, the installer explains the on-device setup and
waits for you to restart, unlock, and confirm it before continuing.
Choose a device from the numbered list. Only paired, physical iPhones and iPads
reported as wired are listed, and the cable connection is checked again before
installation, including after a provisioning build. After each attempt, choose
whether to install another device. Failed attempts return a nonzero exit status.
Wi-Fi-only devices and simulators are excluded. Non-interactive installs skip
device prompts and downloads. `--check` never installs a device.
