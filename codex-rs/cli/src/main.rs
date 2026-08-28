const CODEX_CLI_VERSION: &str = match option_env!("CODEX_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

fn main() -> anyhow::Result<()> {
    codex_cli_runtime::run(CODEX_CLI_VERSION)
}
