"""Exposes the Cargo workspace version to Bazel-built Xedoc binaries."""

def _workspace_package_version(manifest):
    in_workspace_package = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_workspace_package = stripped == "[workspace.package]"
            continue
        if not in_workspace_package:
            continue
        key, _, value = stripped.partition("=")
        if key.strip() == "version":
            return value.strip().strip('"')
    fail("No [workspace.package] version found in the Cargo workspace manifest.")

def _xedoc_workspace_version_impl(rctx):
    version = _workspace_package_version(rctx.read(rctx.attr.cargo_toml))
    rctx.file("BUILD.bazel", 'exports_files(["xedoc_release.env"])\n')
    rctx.file("xedoc_release.env", "XEDOC_RELEASE_VERSION={}\n".format(version))

xedoc_workspace_version = repository_rule(
    implementation = _xedoc_workspace_version_impl,
    attrs = {
        "cargo_toml": attr.label(
            allow_single_file = True,
            mandatory = True,
            doc = "Cargo workspace manifest whose [workspace.package] version is exported.",
        ),
    },
    doc = "Generates a rustc env file carrying the Cargo workspace version, so only binaries that consume it are rebuilt on a version bump.",
)
