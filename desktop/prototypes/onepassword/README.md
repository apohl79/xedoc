# 1Password compatibility probe

An explicitly opt-in macOS experiment, not normal Xedoc Chrome-extension support.
It runs as a separate app with separate browser storage. Do not use a real vault
until you have reviewed the risks: trusting a browser can grant full vault access.
There is no trust bypass.

From the repository root, after `pnpm install`:

```sh
pnpm --filter @xedoc/onepassword-probe download:extension
pnpm --filter @xedoc/onepassword-probe start
pnpm --filter @xedoc/onepassword-probe dist:mac
```

Preparation downloads only the official stable 1Password CRX, pins reviewed
version 8.12.46.29, verifies its CRX3 developer signature against the official
extension ID, and preserves that identity in the extracted manifest. Download
and extraction happen only when explicitly running these commands, not during
dependency installation. Starts rebuild generated `extension/` from the verified
cached CRX. Unmarked existing directories are refused. Generated files live in
`.cache/`, `extension/`, and `dist/`; do not put personal files there.
To refresh, move `.cache/1password.crx` and `extension/` aside and review/update
the pinned hash in `desktop/scripts/prepare-onepassword.cjs`; an upstream release
is not silently trusted.

`dist:mac` uses electron-builder's automatic signing-identity selection; a
suitable local signing identity is required for the desktop trust experiment.
The app is not notarized. Development `start` is not a substitute for testing
the separately signed app. To authorize that app manually, use
**1Password Settings > Browser > Add Browser**. Only the user should complete
trust, unlock, account, and credential/autofill testing.

Use **1Password > Open 1Password** (Cmd+Shift+P), then **Sign in** to open the
official account sign-in page inside the probe. The circle beside **Add account**
is a completion indicator, not a checkbox. HTTPS navigation is restricted to
`1password.com`, `1password.eu`, `1password.ca`, and their subdomains; other
destinations, external SSO, and unrelated pop-ups remain blocked. The account-region
selector can open official regional sign-in pages in another probe window.
Direct account sign-in does not establish that desktop-app integration works. Use only a disposable
test account: the temporary session quits after ten minutes and is removed on
normal exit, so accounts must be added again on the next launch.
The extension is loaded from a copy in that temporary profile, keeping the
signed app bundle unchanged when Chromium updates unpacked-extension metadata.

For the fill check, create a disposable Login whose **website** is the exact
loopback URL shown on the dummy page. Return to the dummy window, open the
extension, and choose **Autofill** for that matching login. Approve 1Password's
HTTP confirmation if prompted. Searching an existing login for a real website
does not make it match the dummy page. **Open & Fill** to unrelated sites is
blocked, including extension-initiated tab updates. Normal Xedoc browser panels
are not connected to this probe.

The extension's own inline controls provide field suggestions and alternate
logins without opening the toolbar popup. Click a login field or use
**1Password > Show autofill suggestions** (Cmd+P); use the arrow keys to select,
Enter to choose, and Escape to dismiss. The test page refreshes once when the
official setup-complete screen appears so its pre-sign-in account state is
discarded. **Reload test page** (Cmd+R) also reinitializes the controls and clears
the dummy fields. If disabled in the extension, use Settings > Autofill & save
to enable field offers, suggestions on field selection, and login suggestions.
These controls remain subject to 1Password's matching and unlock requirements.

The restricted native transport is intended only for 1Password's signed macOS
helper. Diagnostics emit only fixed, allowlisted connection/protocol labels, never
message bodies, arbitrary payload strings, account data, or credentials. The prototype
does not read browser profiles or use the 1Password CLI. Startup refuses
unpatched extension-runtime dependencies; package with the repository's patched
pnpm installation, not a separate npm install.

Evidence so far: the official background initializes and its toolbar action opens
the Welcome screen. Sign in reaches the official account-entry screen; regional
account pages and the EU region-selector flow have also been exercised. The app's
code signature remains valid after extension loading and account-page navigation.
The user confirmed direct account sign-in, vault search, and the matching-login
autofill flow work. The signed probe
reaches the verified native helper, which rejects it with `UnknownBrowser` even
after the manual trust attempt. Desktop unlock remains unverified. A fresh
unauthenticated session verified that the inline shortcut focuses the selected
field and sends the extension's menu key without opening a popup, and that the
official setup-complete route refreshes the test page only once. Saved-login
selection through the inline menu remains acceptance-unverified. In that session,
the official content script collected both dummy inputs and filled them through
its extension-message handler using synthetic disposable values. That verifies
the page-side path, not the complete vault-to-page flow or working password-manager
support. The probe uses a fresh temporary profile, removes it on normal exit,
and quits after ten minutes.
