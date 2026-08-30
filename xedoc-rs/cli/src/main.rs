fn main() -> anyhow::Result<()> {
    xedoc_cli_runtime::run(env!("CARGO_PKG_VERSION"))
}
