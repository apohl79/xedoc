use std::path::Path;
use tempfile::TempDir;
use wiremock::MockServer;
use xedoc_login::XEDOC_API_KEY_ENV_VAR;

pub struct TestXedocExecBuilder {
    home: TempDir,
    cwd: TempDir,
}

impl TestXedocExecBuilder {
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(
            xedoc_utils_cargo_bin::cargo_bin("xedoc-exec")
                .expect("should find binary for xedoc-exec"),
        );
        cmd.current_dir(self.cwd.path())
            .env("XEDOC_HOME", self.home.path())
            .env("XEDOC_SQLITE_HOME", self.home.path())
            .env(XEDOC_API_KEY_ENV_VAR, "dummy");
        cmd
    }
    pub fn cmd_with_server(&self, server: &MockServer) -> assert_cmd::Command {
        let mut cmd = self.cmd();
        let base = format!("{}/v1", server.uri());
        cmd.arg("-c")
            .arg(format!("openai_base_url={}", toml_string_literal(&base)))
            .arg("-c")
            .arg("model_providers.openai.supports_websockets=false");
        cmd
    }

    pub fn cwd_path(&self) -> &Path {
        self.cwd.path()
    }
    pub fn home_path(&self) -> &Path {
        self.home.path()
    }
}

fn toml_string_literal(value: &str) -> String {
    serde_json::to_string(value).expect("serialize TOML string literal")
}

pub fn test_xedoc_exec() -> TestXedocExecBuilder {
    TestXedocExecBuilder {
        home: TempDir::new().expect("create temp home"),
        cwd: TempDir::new().expect("create temp cwd"),
    }
}
