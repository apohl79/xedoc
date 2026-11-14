# Xedoc for macOS

Native Bonjour host discovery and independent WebKit host windows. Requires
macOS 14 or newer. The icon and discovery implementation are shared with iOS.

```sh
xcodegen generate --spec macos/project.yml
open macos/Xedoc.xcodeproj
```

Choose a host or enter its HTTPS origin. Hosts… (`⌘N`) opens discovery without
disconnecting existing windows. Authenticated hosts reopen on launch; without a
saved login, discovery opens first. Each origin has its own persistent website
data store, including when hosts share a hostname but use different ports.
Certificates use normal system trust; discovery does not grant trust.
Host windows have edge-to-edge session tabs, native window controls, and no
browser toolbar. Reload Host (`⌘R`) reloads the active connection.

Allow notifications when connecting. Input/approval and turn-end alerts contain
only host/project names. Clicking an alert opens its exact session, after
sign-in if needed. An alert cannot navigate a different account. Notifications
require the app to remain running and an updated host with the native routing
bridge; quitting the app stops local notifications.

The standard release script includes a universal Developer ID-signed app ZIP
and requires notarization before publishing it. It staples the app before
packaging, verifies Gatekeeper, and updates `apohl79/homebrew-xedoc` with the
uploaded stable archive’s SHA-256. `--skip-macos-app` omits it.

```sh
scripts/build_xedoc_release.sh --macos-app-only
brew install --cask apohl79/xedoc/xedoc
```

The Homebrew install requires a published release containing
`xedoc-macos-VERSION.zip`. For a local, signed build without publishing:

```sh
python3 scripts/build_xedoc_release.py --ref HEAD --macos-app-only \
  --skip-github-release --allow-dirty
```

Add `--notarize --notarytool-keychain-profile PROFILE` to notarize locally.
