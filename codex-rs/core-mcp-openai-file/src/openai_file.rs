use codex_api::OPENAI_FILE_UPLOAD_LIMIT_BYTES;
use codex_api::upload_openai_file;
use codex_core_environment::TurnEnvironment;
use codex_http_client::HttpClientFactory;
use codex_login::CodexAuth;
use codex_utils_path_uri::PathUri;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

/// Runtime data needed to rewrite a Codex Apps tool's local file arguments.
pub struct OpenAiFileUploadContext<'a> {
    /// Authentication used for the OpenAI file-storage request.
    pub auth: Option<&'a CodexAuth>,
    /// Selected environment from which local files are read.
    pub primary_environment: Option<&'a TurnEnvironment>,
    /// ChatGPT API base URL used for file-storage requests.
    pub chatgpt_base_url: &'a str,
    /// Factory used to build HTTP clients for file-storage requests.
    pub http_client_factory: &'a HttpClientFactory,
}

/// Uploads declared Apps file arguments and rewrites them to the provided-file payload shape.
pub async fn rewrite_mcp_tool_arguments_for_openai_files(
    context: &OpenAiFileUploadContext<'_>,
    arguments_value: Option<JsonValue>,
    openai_file_input_optional_fields: Option<&HashMap<String, Vec<String>>>,
) -> Result<Option<JsonValue>, String> {
    let Some(openai_file_input_optional_fields) = openai_file_input_optional_fields else {
        return Ok(arguments_value);
    };

    let Some(arguments_value) = arguments_value else {
        return Ok(None);
    };
    let Some(arguments) = arguments_value.as_object() else {
        return Ok(Some(arguments_value));
    };
    let mut rewritten_arguments = arguments.clone();

    for (field_name, optional_fields) in openai_file_input_optional_fields {
        let Some(value) = arguments.get(field_name) else {
            continue;
        };
        let Some(uploaded_value) =
            rewrite_argument_value_for_openai_files(context, field_name, optional_fields, value)
                .await?
        else {
            continue;
        };
        rewritten_arguments.insert(field_name.clone(), uploaded_value);
    }

    if rewritten_arguments == *arguments {
        return Ok(Some(arguments_value));
    }

    Ok(Some(JsonValue::Object(rewritten_arguments)))
}

async fn rewrite_argument_value_for_openai_files(
    context: &OpenAiFileUploadContext<'_>,
    field_name: &str,
    optional_fields: &[String],
    value: &JsonValue,
) -> Result<Option<JsonValue>, String> {
    match value {
        JsonValue::String(file_path) => {
            let rewritten = build_uploaded_argument_value(
                context,
                field_name,
                /*index*/ None,
                optional_fields,
                file_path,
            )
            .await?;
            Ok(Some(rewritten))
        }
        JsonValue::Array(values) => {
            let mut rewritten_values = Vec::with_capacity(values.len());
            for (index, item) in values.iter().enumerate() {
                let Some(file_path) = item.as_str() else {
                    return Ok(None);
                };
                let rewritten = build_uploaded_argument_value(
                    context,
                    field_name,
                    Some(index),
                    optional_fields,
                    file_path,
                )
                .await?;
                rewritten_values.push(rewritten);
            }
            Ok(Some(JsonValue::Array(rewritten_values)))
        }
        _ => Ok(None),
    }
}

async fn build_uploaded_argument_value(
    context: &OpenAiFileUploadContext<'_>,
    field_name: &str,
    index: Option<usize>,
    optional_fields: &[String],
    file_path: &str,
) -> Result<JsonValue, String> {
    let contextualize_error = |error: String| match index {
        Some(index) => {
            format!("failed to upload `{file_path}` for `{field_name}[{index}]`: {error}")
        }
        None => format!("failed to upload `{file_path}` for `{field_name}`: {error}"),
    };
    let Some(auth) = context.auth else {
        return Err("ChatGPT auth is required to upload files for Codex Apps tools".to_string());
    };
    if !auth.uses_codex_backend() {
        return Err("ChatGPT auth is required to upload files for Codex Apps tools".to_string());
    }
    let Some(turn_environment) = context.primary_environment else {
        return Err(contextualize_error(
            "no primary turn environment is available".to_string(),
        ));
    };
    let native_environment_cwd = turn_environment
        .cwd()
        .to_abs_path()
        .map_err(|error| contextualize_error(error.to_string()))?;
    let resolved_path = native_environment_cwd.join(file_path);
    let path_uri = PathUri::from_abs_path(&resolved_path);
    let fs = turn_environment.environment.get_filesystem();
    let metadata = fs
        .get_metadata(&path_uri, /*sandbox*/ None)
        .await
        .map_err(|error| contextualize_error(error.to_string()))?;
    if !metadata.is_file {
        return Err(contextualize_error(format!(
            "path `{}` is not a file",
            resolved_path.display()
        )));
    }
    if metadata.size > OPENAI_FILE_UPLOAD_LIMIT_BYTES {
        return Err(contextualize_error(format!(
            "file `{}` is too large: {} bytes exceeds the limit of {} bytes",
            resolved_path.display(),
            metadata.size,
            OPENAI_FILE_UPLOAD_LIMIT_BYTES,
        )));
    }
    let contents = fs
        .read_file_stream(&path_uri, /*sandbox*/ None)
        .await
        .map_err(|error| contextualize_error(error.to_string()))?;
    let file_name = resolved_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file")
        .to_string();
    let upload_auth = codex_model_provider::auth_provider_from_auth(auth);
    let uploaded = upload_openai_file(
        context.chatgpt_base_url.trim_end_matches('/'),
        upload_auth.as_ref(),
        context.http_client_factory,
        file_name,
        metadata.size,
        contents,
    )
    .await
    .map_err(|error| contextualize_error(error.to_string()))?;
    let mut payload = serde_json::Map::new();
    payload.insert(
        "download_url".to_string(),
        JsonValue::String(uploaded.download_url),
    );
    payload.insert("file_id".to_string(), JsonValue::String(uploaded.file_id));
    if optional_fields
        .iter()
        .any(|optional_field| optional_field == "mime_type")
        && let Some(mime_type) = uploaded.mime_type
    {
        payload.insert("mime_type".to_string(), JsonValue::String(mime_type));
    }
    if optional_fields
        .iter()
        .any(|optional_field| optional_field == "file_name")
    {
        payload.insert(
            "file_name".to_string(),
            JsonValue::String(uploaded.file_name),
        );
    }
    Ok(JsonValue::Object(payload))
}
