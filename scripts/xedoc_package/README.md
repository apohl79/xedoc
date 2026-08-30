# Xedoc package builder

This package contains the implementation behind `scripts/build_xedoc_package.py`.
The top-level script is the stable executable entry point; these modules keep the
package-building logic split by responsibility.

The builder creates a canonical Xedoc package directory:

```text
.
├── xedoc-package.json
├── bin
│   ├── <entrypoint>[.exe]
│   └── xedoc-session                    # Unix fork packages only
├── xedoc-resources
│   └── bwrap                             # Linux only
└── xedoc-path
    └── rg[.exe]
```

The package directory is the primary artifact. Archive formats such as
`.tar.gz`, `.tar.zst`, and `.zip` are serializations of that directory.

If `--target` is omitted, the builder uses the release target for the current
host platform. On Linux, that default is a musl target to match Xedoc release
artifacts; pass a GNU Linux target explicitly for native glibc local builds. If
`--package-dir` is omitted, the builder creates a new temporary directory and
prints its path after the package is built.

The `--variant` flag selects the package entrypoint. Supported variants are
`xedoc` and `xedoc-app-server`. The `version` field in `xedoc-package.json` is
read from `[workspace.package].version` in `xedoc-rs/Cargo.toml`.

Pass `--include-session-control` for the fork package variant to include the
Unix `xedoc-session` control CLI; this uses package layout version 2.

## Source-built artifacts

Artifacts built from this repository are built by the package builder in one
grouped `cargo build` command per package when they are needed and no prebuilt
override was provided:

- all targets: the selected entrypoint, unless `--entrypoint-bin` is provided
- Linux targets: `bwrap`, unless `--bwrap-bin` is provided

The default cargo profile is `dev-small` because local iteration should favor
fast, small builds. Release jobs should pass `--cargo-profile release` and an
explicit target. Release jobs that already built and signed/notarized the
entrypoint should pass `--entrypoint-bin` so the package contains that exact
binary instead of rebuilding it.

Release jobs that already built package resource binaries should also pass the
corresponding resource flags: `--bwrap-bin` for Linux packages. This keeps
package archive creation as a pure staging step after signing instead of
rebuilding resources.

`rg` is not built from this repository, so the builder fetches it from the
DotSlash manifest at `scripts/xedoc_package/rg`. Downloaded archives are cached
under `$TMPDIR/xedoc-package/<target>-rg` and are reused only after the recorded
size and SHA-256 digest have been verified. Pass `--rg-bin` to use a local
ripgrep executable instead.
