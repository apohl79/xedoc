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
