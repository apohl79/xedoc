// Bazel reports 0.0.0 as CARGO_PKG_VERSION; the workspace version arrives via rustc_env_files.
const XEDOC_CLI_VERSION: &str = match option_env!("XEDOC_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

fn main() -> anyhow::Result<()> {
    xedoc_cli_runtime::run(XEDOC_CLI_VERSION)
}
