# Bundled Aile

`xedoc-rs/app-server/src/web_ui_assets/XedocAile-Regular.woff2` is a custom
Iosevka Aile build with a 500/1000-em base width, matching Iosevka Term's base
width instead of upstream Aile's 600. It retains quasi-proportional spacing;
individual letters and whole strings need not match monospace text widths.
The internal family name is `Xedoc Aile`; CSS exposes it as `Iosevka Aile`.
Its distinct asset name avoids reusing browsers' cached upstream Aile font.

Build with Node.js and npm from Iosevka **v34.9.1**, commit
`cea353b17b9b5c6e22da4f0fcae66f16d68a4edf`:

```sh
git clone --branch v34.9.1 --depth 1 https://github.com/be5invis/Iosevka.git
cp private-build-plans.toml Iosevka/private-build-plans.toml
cd Iosevka
npm ci
npm run build -- woff2-unhinted::XedocAile --jCmd=2
```

Copy `dist/XedocAile/WOFF2-Unhinted/XedocAile-Regular.woff2` to the asset path
above. The package builder includes this same asset in the remote-agent payload;
the Rust app-server embeds it directly. The accompanying `Iosevka-LICENSE.txt`
contains attribution and the SIL Open Font License.
