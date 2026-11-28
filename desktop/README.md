# Xedoc desktop

Shared Electron app for macOS, Linux and Windows: mDNS host discovery, one
window per host, paired-device access, notifications, and Chromium website panels.

```sh
pnpm install
pnpm --filter xedoc-desktop start
pnpm --filter xedoc-desktop run dist:mac     # universal macOS ZIP
pnpm --filter xedoc-desktop run dist:linux   # dist/*.AppImage and dist/*.deb
pnpm --filter xedoc-desktop run dist:win     # dist/*.exe (NSIS) and dist/*-win.zip
```

Choose a host or enter its HTTPS origin (HTTP only for localhost). Hosts…
(`Ctrl+Shift+N`) opens discovery without disconnecting existing windows.
“This computer” connects to the Host GUI on the same machine through
`$XEDOC_HOME/host-gui/local.json` (`~/.xedoc` by default) without a login or
pairing. Authenticated hosts reopen on launch; without a saved login,
discovery opens first. Each origin has its own persistent session data.
Certificates use normal system trust; discovery does not grant trust.
Discovery needs mDNS (UDP 5353) allowed through the firewall.
On macOS it uses the native network discovery service through a small packaged
helper. Developing or packaging the macOS app requires Xcode command-line tools;
the installed app does not compile Swift or require developer tools.

Linux and Windows shortcuts use `Ctrl+Shift` so plain `Ctrl` keys reach the terminal:
Hosts… (`N`), New Tab (`T`, opens the session “+” menu), Reload Host (`R`),
Open Website… (`L`), and Quit (`Q`). There is no menu bar; right-click the
host page for the same commands and Pair Device…. `Ctrl`+click opens a link in
the external browser. Closing the last window quits the app on Linux and Windows.
Use **Extensions → Load gTabs Extension…** to select gTabs' unpacked `extension/`
folder. Xedoc remembers the folder across launches. Reload open gTabs and Ultimate
Guitar pages after loading or reloading it; use the same menu to remove it.
macOS uses a native File menu with `⌘N`, `⌘T`, `⌘R` and `⌘L`; closing the last
window keeps the app running. See `macos/README.md` for signed macOS releases.
`⌘+`, `⌘-` and `⌘0` on macOS (`Ctrl` elsewhere) increase, decrease and reset
the app-wide UI size, including website panels. The size persists across launches.

The first macOS launch imports only safe host/session preferences from the
legacy Swift app, preserving existing Electron settings. It does not import
cookies, device keys or certificates. Sign in again where needed and pair remote
hosts again; same-machine Host GUI access still requires neither.

Pair Device… stores the device key in the app’s user data directory
(`paired-hosts.json`). The key is encrypted with the OS keystore (`safeStorage`)
when available; otherwise it is stored unencrypted with mode `0600`. Paired
hosts must use a non-loopback hostname.

Allow notifications when asked by the desktop environment. Alerts behave as in
the macOS app: only host/project names, only for the signed-in user, and a
click opens the exact session. Notifications stop when the app quits.

The `.deb` depends on GTK 3, libnotify, NSS, libsecret, and ALSA. Windows
builds are unsigned.

The standard release script builds and uploads
`xedoc-desktop-VERSION-linux-{x86_64,aarch64}.{AppImage,deb}` and
`xedoc-desktop-VERSION-windows-x86_64-setup.exe`. `--skip-desktop-app` omits
them, and `--desktop-app-only` builds only them:

```sh
python3 scripts/build_xedoc_release.py --ref HEAD --desktop-app-only \
  --skip-github-release --allow-dirty
```

On Linux, `scripts/install/install.sh` offers to install the app after the CLI.
It uses the `.deb` through `apt-get` when available and otherwise installs the
AppImage as `~/.local/bin/xedoc-desktop` with a launcher entry. On Windows,
`scripts/install/install_windows_app.ps1` installs the latest release for the
current user.
