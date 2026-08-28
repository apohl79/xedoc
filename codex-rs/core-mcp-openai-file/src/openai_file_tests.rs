use super::OpenAiFileUploadContext;
use super::openai_file::build_uploaded_argument_value;
use super::openai_file::rewrite_argument_value_for_openai_files;
use super::rewrite_mcp_tool_arguments_for_openai_files;
use codex_api::OPENAI_FILE_UPLOAD_LIMIT_BYTES;
use codex_core_environment::TurnEnvironment;
use codex_exec_server::Environment;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::CodexAuth;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn test_turn_environment(cwd: &Path) -> TurnEnvironment {
    let cwd = AbsolutePathBuf::try_from(cwd).expect("absolute path");
    TurnEnvironment::new(
        "local".to_string(),
        Arc::new(Environment::default_for_tests()),
        PathUri::from_abs_path(&cwd),
        Vec::new(),
        /*shell*/ None,
    )
}

fn test_http_client_factory() -> HttpClientFactory {
    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
}

#[tokio::test]
async fn openai_file_argument_rewrite_requires_declared_file_params() {
    let http_client_factory = test_http_client_factory();
    let context = OpenAiFileUploadContext {
        auth: None,
        primary_environment: None,
        chatgpt_base_url: "",
        http_client_factory: &http_client_factory,
    };
    let arguments = Some(serde_json::json!({
        "file": "/tmp/codex-smoke-file.txt"
    }));

    let rewritten = rewrite_mcp_tool_arguments_for_openai_files(
        &context,
        arguments.clone(),
        /*openai_file_input_optional_fields*/ None,
    )
    .await
    .expect("rewrite should succeed");

    assert_eq!(rewritten, arguments);
}

#[tokio::test]
async fn build_uploaded_argument_value_includes_schema_declared_optional_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/files"))
        .and(header("chatgpt-account-id", "account_id"))
        .and(body_json(serde_json::json!({
            "file_name": "file_report.csv",
            "file_size": 5,
            "use_case": "codex",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "file_id": "file_123",
            "upload_url": format!("{}/upload/file_123", server.uri()),
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/file_123"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/files/file_123/uploaded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success",
            "download_url": format!("{}/download/file_123", server.uri()),
            "file_name": "file_report.csv",
            "mime_type": "text/csv",
            "file_size_bytes": 5,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let dir = tempdir().expect("temp dir");
    let local_path = dir.path().join("file_report.csv");
    tokio::fs::write(&local_path, b"hello")
        .await
        .expect("write local file");
    let environment = test_turn_environment(dir.path());
    let http_client_factory = test_http_client_factory();
    let base_url = format!("{}/backend-api", server.uri());
    let context = OpenAiFileUploadContext {
        auth: Some(&auth),
        primary_environment: Some(&environment),
        chatgpt_base_url: &base_url,
        http_client_factory: &http_client_factory,
    };

    let rewritten = build_uploaded_argument_value(
        &context,
        "file",
        /*index*/ None,
        &["mime_type".to_string(), "file_name".to_string()],
        "file_report.csv",
    )
    .await
    .expect("rewrite should upload the local file");

    assert_eq!(
        rewritten,
        serde_json::json!({
            "download_url": format!("{}/download/file_123", server.uri()),
            "file_id": "file_123",
            "mime_type": "text/csv",
            "file_name": "file_report.csv",
        })
    );
}

#[tokio::test]
async fn build_uploaded_argument_value_rejects_oversized_file_before_reading() {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let dir = tempdir().expect("temp dir");
    let file_path = dir.path().join("oversized.bin");
    let file = std::fs::File::create(&file_path).expect("create sparse file");
    file.set_len(OPENAI_FILE_UPLOAD_LIMIT_BYTES + 1)
        .expect("size sparse file");
    let environment = test_turn_environment(dir.path());
    let http_client_factory = test_http_client_factory();
    let context = OpenAiFileUploadContext {
        auth: Some(&auth),
        primary_environment: Some(&environment),
        chatgpt_base_url: "https://chatgpt.com/backend-api",
        http_client_factory: &http_client_factory,
    };

    let error =
        build_uploaded_argument_value(&context, "file", /*index*/ None, &[], "oversized.bin")
            .await
            .expect_err("oversized file should be rejected");

    assert!(error.contains("is too large"));
    assert!(error.contains(&(OPENAI_FILE_UPLOAD_LIMIT_BYTES + 1).to_string()));
}

#[tokio::test]
async fn rewrite_argument_value_for_openai_files_rewrites_array_paths() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/files"))
        .and(header("chatgpt-account-id", "account_id"))
        .and(body_json(serde_json::json!({
            "file_name": "one.csv",
            "file_size": 3,
            "use_case": "codex",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "file_id": "file_1",
            "upload_url": format!("{}/upload/file_1", server.uri()),
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/files"))
        .and(header("chatgpt-account-id", "account_id"))
        .and(body_json(serde_json::json!({
            "file_name": "two.csv",
            "file_size": 3,
            "use_case": "codex",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "file_id": "file_2",
            "upload_url": format!("{}/upload/file_2", server.uri()),
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/file_1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/file_2"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/files/file_1/uploaded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success",
            "download_url": format!("{}/download/file_1", server.uri()),
            "file_name": "one.csv",
            "mime_type": "text/csv",
            "file_size_bytes": 3,
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/files/file_2/uploaded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "success",
            "download_url": format!("{}/download/file_2", server.uri()),
            "file_name": "two.csv",
            "mime_type": "text/csv",
            "file_size_bytes": 3,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let dir = tempdir().expect("temp dir");
    tokio::fs::write(dir.path().join("one.csv"), b"one")
        .await
        .expect("write first local file");
    tokio::fs::write(dir.path().join("two.csv"), b"two")
        .await
        .expect("write second local file");
    let environment = test_turn_environment(dir.path());
    let http_client_factory = test_http_client_factory();
    let base_url = format!("{}/backend-api", server.uri());
    let context = OpenAiFileUploadContext {
        auth: Some(&auth),
        primary_environment: Some(&environment),
        chatgpt_base_url: &base_url,
        http_client_factory: &http_client_factory,
    };

    let rewritten = rewrite_argument_value_for_openai_files(
        &context,
        "files",
        &[],
        &serde_json::json!(["one.csv", "two.csv"]),
    )
    .await
    .expect("rewrite should succeed");

    assert_eq!(
        rewritten,
        Some(serde_json::json!([
            {
                "download_url": format!("{}/download/file_1", server.uri()),
                "file_id": "file_1",
            },
            {
                "download_url": format!("{}/download/file_2", server.uri()),
                "file_id": "file_2",
            }
        ]))
    );
}

#[tokio::test]
async fn rewrite_mcp_tool_arguments_for_openai_files_surfaces_upload_failures() {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let dir = tempdir().expect("temp dir");
    let environment = test_turn_environment(dir.path());
    let http_client_factory = test_http_client_factory();
    let context = OpenAiFileUploadContext {
        auth: Some(&auth),
        primary_environment: Some(&environment),
        chatgpt_base_url: "https://chatgpt.com/backend-api",
        http_client_factory: &http_client_factory,
    };
    let error = rewrite_mcp_tool_arguments_for_openai_files(
        &context,
        Some(serde_json::json!({
            "file": "/definitely/missing/file.csv",
        })),
        Some(&HashMap::from([("file".to_string(), Vec::new())])),
    )
    .await
    .expect_err("missing file should fail");

    assert!(error.contains("failed to upload"));
    assert!(error.contains("file"));
}
