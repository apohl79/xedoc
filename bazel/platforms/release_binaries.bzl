"""Rules for building release binaries across supported target platforms."""

load("@rules_platform//platform_data:defs.bzl", "platform_data")

# Windows release binaries use the gnullvm platform because no Rust toolchain is
# registered for the MSVC target; they are published under the MSVC target name.
PLATFORMS = {
    "linux_arm64_gnu.2.28": "@llvm//platforms:linux_arm64_gnu.2.28",
    "linux_amd64_gnu.2.28": "@llvm//platforms:linux_amd64_gnu.2.28",
    "macos_amd64": "@llvm//platforms:macos_amd64",
    "macos_arm64": "@llvm//platforms:macos_arm64",
    "windows_x86_64_gnullvm": "//:windows_x86_64_gnullvm",
}

def multiplatform_binaries(name, platforms = PLATFORMS):
    for suffix, platform in platforms.items():
        platform_data(
            name = name + "_" + suffix,
            platform = platform,
            target = name,
            tags = ["manual"],
        )

    native.filegroup(
        name = "release_binaries",
        srcs = [name + "_" + suffix for suffix in platforms],
        tags = ["manual"],
    )
