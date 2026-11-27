//! Transfers captured browser downloads into the calling thread's execution environment.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;
use xedoc_core::ThreadConfigSnapshot;
use xedoc_exec_server::CreateDirectoryOptions;
use xedoc_exec_server::EnvironmentManager;
use xedoc_exec_server::FileSystemSandboxContext;

const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 512 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadChunk {
    download_id: String,
    filename: String,
    mime_type: String,
    size: usize,
    offset: usize,
    base64: String,
    eof: bool,
}

pub(crate) async fn save(
    home: &Path,
    thread_id: &str,
    arguments: Value,
    snapshot: ThreadConfigSnapshot,
    environments: &EnvironmentManager,
) -> Result<Value, String> {
    let panel_id = arguments
        .get("panelId")
        .and_then(Value::as_str)
        .ok_or("A website panelId is required")?;
    let download_id = arguments
        .get("downloadId")
        .and_then(Value::as_str)
        .filter(|value| value.len() == 36 && Uuid::parse_str(value).is_ok())
        .ok_or("A valid downloadId is required")?;
    let environment_id = match arguments.get("environmentId") {
        None => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 1024 => {
            Some(value.as_str())
        }
        Some(_) => return Err("environmentId must be a bounded nonempty string".to_string()),
    };
    let selections = snapshot.environment_selections();
    let selection = match environment_id {
        Some(id) => selections
            .iter()
            .find(|selection| selection.environment_id == id)
            .ok_or("environmentId must name an execution environment selected by this thread")?,
        None => match selections {
            [selection] => selection,
            [] => return Err("This thread has no selected execution environment".to_string()),
            _ => {
                return Err(
                    "Choose environmentId from this thread's selected execution environments"
                        .to_string(),
                );
            }
        },
    };
    let environment = environments
        .get_environment(&selection.environment_id)
        .ok_or("The selected execution environment is unavailable")?;
    let filesystem = environment.get_filesystem();
    let mut sandbox = FileSystemSandboxContext::from_permission_profile_with_cwd(
        snapshot.permission_profile.clone(),
        selection.cwd.clone(),
    );
    sandbox.workspace_roots = selection.workspace_roots.clone();

    let mut contents = Vec::new();
    let mut metadata: Option<(String, String, usize)> = None;
    loop {
        let result = crate::browser_tools::request(
            home,
            thread_id,
            "save_download",
            json!({
                "panelId": panel_id,
                "downloadId": download_id,
                "offset": contents.len(),
            }),
        )
        .await?;
        let chunk: DownloadChunk = serde_json::from_value(result)
            .map_err(|_| "Invalid browser download chunk".to_string())?;
        if chunk.download_id != download_id
            || chunk.offset != contents.len()
            || chunk.size > MAX_FILE_BYTES
            || chunk.filename.is_empty()
            || chunk.filename.len() > 1024
            || chunk.mime_type.len() > 256
            || chunk.base64.len() > MAX_CHUNK_BYTES.div_ceil(/*rhs*/ 3) * 4
        {
            return Err("Invalid browser download metadata or chunk bounds".to_string());
        }
        let identity = (chunk.filename, chunk.mime_type, chunk.size);
        if let Some(expected) = &metadata {
            if expected != &identity {
                return Err("Browser download metadata changed during transfer".to_string());
            }
        } else {
            contents.reserve(chunk.size);
            metadata = Some(identity);
        }
        let bytes = STANDARD
            .decode(chunk.base64)
            .map_err(|_| "Browser download chunk is not valid base64".to_string())?;
        let end = contents.len() + bytes.len();
        if bytes.len() > MAX_CHUNK_BYTES
            || end > chunk.size
            || chunk.eof != (end == chunk.size)
            || (bytes.is_empty() && !chunk.eof)
        {
            return Err("Browser download chunk is truncated, stalled or oversized".to_string());
        }
        contents.extend(bytes);
        if chunk.eof {
            break;
        }
    }
    let (filename, mime_type, size) = metadata.ok_or("Browser download metadata is missing")?;
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or("download");
    let basename: String = basename
        .chars()
        .take(/*n*/ 60)
        .map(|character| {
            if character.is_control() || r#"\/:*?"<>|"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let basename = basename.trim_matches(['.', ' ']);
    let basename = if basename.is_empty() {
        "download"
    } else {
        basename
    };
    // A fixed prefix also prevents Windows device names such as CON or COM1.
    let filename = format!("download-{basename}");
    let directory = selection
        .cwd
        .join(&format!(".xedoc-downloads/{}", Uuid::now_v7()))
        .map_err(|error| format!("Cannot resolve browser download directory: {error}"))?;
    let path = directory
        .join(&filename)
        .map_err(|error| format!("Cannot resolve browser download path: {error}"))?;
    let result = json!({
        "downloadId": download_id,
        "filename": filename,
        "mimeType": mime_type,
        "size": size,
        "path": path,
        "environmentId": selection.environment_id,
    });
    if result.to_string().len() > 8192 {
        return Err("Browser download metadata exceeds 8 KiB".to_string());
    }
    filesystem
        .create_directory(
            &directory,
            CreateDirectoryOptions { recursive: true },
            Some(&sandbox),
        )
        .await
        .map_err(|error| format!("Cannot create browser download directory: {error}"))?;
    filesystem
        .write_file(&path, contents, Some(&sandbox))
        .await
        .map_err(|error| format!("Cannot save browser download: {error}"))?;
    Ok(result)
}
