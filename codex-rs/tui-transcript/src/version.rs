use std::sync::OnceLock;

static CODEX_CLI_VERSION: OnceLock<&'static str> = OnceLock::new();

/// Sets the release version supplied by the stamped executable entrypoint.
pub fn set_codex_cli_version(version: &'static str) {
    let configured_version = CODEX_CLI_VERSION.get_or_init(|| version);
    assert_eq!(
        *configured_version, version,
        "Codex CLI version must not change while the process is running"
    );
}

pub fn codex_cli_version() -> &'static str {
    CODEX_CLI_VERSION
        .get()
        .copied()
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}
