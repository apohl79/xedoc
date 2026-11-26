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
the pinned hash in `prepare.cjs`; an upstream release is not silently trusted.

`dist:mac` uses electron-builder's automatic signing-identity selection; a
suitable local signing identity is required for the desktop trust experiment.
The app is not notarized. Development `start` is not a substitute for testing
the separately signed app. To authorize that app manually, use
**1Password Settings > Browser > Add Browser**. Only the user should complete
trust, unlock, account, and credential/autofill testing.

The restricted native transport is intended only for 1Password's signed macOS
helper. Diagnostics emit only fixed, allowlisted connection/protocol labels, never
message bodies, arbitrary payload strings, account data, or credentials. The prototype
does not read browser profiles or use the 1Password CLI. Startup refuses
unpatched extension-runtime dependencies; package with the repository's patched
pnpm installation, not a separate npm install.

Evidence so far: the official background initializes and its toolbar action opens
the Welcome screen. The signed probe reaches the verified native helper, which
rejects it with `UnknownBrowser` even after the manual trust attempt. Unlock and
autofill remain unverified; this does not establish working password-manager
support. The probe uses a fresh temporary profile, removes it on normal exit,
and quits after ten minutes.
