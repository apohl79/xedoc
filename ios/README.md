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

These are connected-app alerts, not APNs push notifications. They work in the
foreground and while the app can still process host events in the background.
iOS suspension, disconnection, or closing the app stops new alerts. Opening a
session does not generate alerts for its existing state.

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
unexpired provisioning profile. To add a device, register it and rebuild the
release; the installer does not register devices or change signing accounts.

On macOS, the CLI installer offers optional iPhone/iPad installation. To install
only the mobile app without changing the desktop installation:

```sh
sh scripts/install/install.sh --ios-only
# Or use a previously downloaded/built artifact:
sh scripts/install/install.sh --ios-only --local-ios-zip /path/to/xedoc-ios-VERSION.zip
```

The installer verifies the GitHub download's SHA-256 digest before extracting it.
With full Xcode and Python 3 installed, accept the prompt, connect and unlock a
device by USB cable, trust the Mac, and enable Developer Mode when required.
Choose a device from the numbered list. Only paired, physical iPhones and iPads
reported as wired are listed, and the cable connection is checked again before
installation. After each attempt, choose whether to install another device.
Wi-Fi-only devices and simulators are excluded. Non-interactive installs skip
device prompts and downloads. `--check` never installs a device.
