use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_login::CLIENT_ID;
use xedoc_login::REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR;

fn xedoc_command(xedoc_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(xedoc_utils_cargo_bin::cargo_bin("xedoc")?);
    cmd.env("XEDOC_HOME", xedoc_home);
    Ok(cmd)
}

fn write_file_auth_config(xedoc_home: &Path) -> Result<()> {
    std::fs::write(
        xedoc_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    Ok(())
}

fn read_auth_json(xedoc_home: &Path) -> Result<Value> {
    let auth_json = std::fs::read_to_string(xedoc_home.join("auth.json"))?;
    Ok(serde_json::from_str(&auth_json)?)
}

#[test]
fn login_with_api_key_reads_stdin_and_writes_auth_json() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    write_file_auth_config(xedoc_home.path())?;

    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.args([
        "-c",
        "forced_login_method=\"api\"",
        "login",
        "--with-api-key",
    ])
    .write_stdin("sk-test\n")
    .assert()
    .success()
    .stderr(contains("Successfully logged in"));

    let auth = read_auth_json(xedoc_home.path())?;
    assert_eq!(auth["OPENAI_API_KEY"], "sk-test");
    assert!(auth.get("tokens").is_none());
    assert!(auth.get("agent_identity").is_none());

    Ok(())
}

#[test]
fn login_with_access_token_rejects_invalid_jwt() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    write_file_auth_config(xedoc_home.path())?;

    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.args(["login", "--with-access-token"])
        .write_stdin("not-a-jwt\n")
        .assert()
        .failure()
        .stderr(contains("Error logging in with access token"));

    Ok(())
}

#[test]
fn anthropic_import_migrates_a_legacy_credential() -> Result<()> {
    let xedoc_home = TempDir::new()?;
    let source = TempDir::new()?;
    std::fs::write(
        source.path().join("claude-user@example.com.json"),
        serde_json::to_vec(&json!({
            "access_token": "anthropic-access",
            "refresh_token": "anthropic-refresh",
            "email": "user@example.com",
            "expired": "2030-01-01T00:00:00.000Z",
            "account_uuid": "anthropic-account",
            "type": "claude",
        }))?,
    )?;

    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.args([
        "login",
        "anthropic",
        "import",
        source
            .path()
            .to_str()
            .context("source path must be UTF-8")?,
    ])
    .assert()
    .success()
    .stderr(contains("Imported 1 Anthropic account(s)"));

    let imported = std::fs::read_to_string(
        xedoc_home
            .path()
            .join("providers/anthropic/accounts/anthropic-user@example.com.json"),
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&imported)?,
        json!({
            "access_token": "anthropic-access",
            "refresh_token": "anthropic-refresh",
            "email": "user@example.com",
            "expires_at": "2030-01-01T00:00:00.000Z",
            "account_id": "anthropic-account",
            "last_refresh_at": null,
            "type": "anthropic",
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_login_revokes_existing_auth_before_requesting_new_tokens() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": "0",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "authorization-code-123",
            "code_challenge": "code-challenge-123",
            "code_verifier": "code-verifier-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
            "access_token": "new-access",
            "refresh_token": "new-refresh",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let xedoc_home = TempDir::new()?;
    write_file_auth_config(xedoc_home.path())?;
    std::fs::write(
        xedoc_home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "account_id": "old-account",
            },
        }))?,
    )?;

    let issuer = server.uri();
    let mut cmd = xedoc_command(xedoc_home.path())?;
    cmd.env(
        REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{issuer}/oauth/revoke"),
    )
    .env("NO_PROXY", "127.0.0.1,localhost")
    .env("no_proxy", "127.0.0.1,localhost")
    .env_remove("XEDOC_ACCESS_TOKEN")
    .env_remove("OPENAI_API_KEY")
    .args(["login", "--device-auth", "--experimental_issuer", &issuer])
    .assert()
    .success()
    .stderr(contains("Successfully logged in"));

    let requests = server
        .received_requests()
        .await
        .context("failed to read mock OAuth requests")?;
    let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
    assert_eq!(
        paths,
        vec![
            "/oauth/revoke",
            "/api/accounts/deviceauth/usercode",
            "/api/accounts/deviceauth/token",
            "/oauth/token",
        ]
    );
    assert_eq!(
        requests[0]
            .body_json::<Value>()
            .context("revoke request should be JSON")?,
        json!({
            "token": "old-refresh",
            "token_type_hint": "refresh_token",
            "client_id": CLIENT_ID,
        })
    );

    let auth = read_auth_json(xedoc_home.path())?;
    assert_eq!(auth["tokens"]["refresh_token"], "new-refresh");
    Ok(())
}
