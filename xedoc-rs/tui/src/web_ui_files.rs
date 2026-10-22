//! Bounded browser file uploads kept alive for the session's file references.

use std::io;

use base64::Engine;

pub(crate) struct UploadedDocument {
    pub(crate) _directory: tempfile::TempDir,
    pub(crate) path: std::path::PathBuf,
    pub(crate) name: String,
    pub(crate) anchor: String,
    pub(crate) preview: String,
    pub(crate) mime_type: &'static str,
    pub(crate) upload_id: String,
}

pub(crate) fn upload_document(name: String, data_url: String) -> io::Result<UploadedDocument> {
    let (_, data) = data_url
        .split_once(',')
        .filter(|(header, _)| header.starts_with("data:") && header.ends_with(";base64"))
        .ok_or_else(|| io::Error::other("invalid file upload"))?;
    if data.len() > 11_184_812 {
        return Err(io::Error::other("file exceeds 8 MiB"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(io::Error::other)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("file exceeds 8 MiB"));
    }
    let filename: String = name
        .chars()
        .take(120)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let filename = if filename.is_empty() || matches!(filename.as_str(), "." | "..") {
        "document".to_string()
    } else {
        filename
    };
    let directory = tempfile::Builder::new().prefix("xedoc-webui-").tempdir()?;
    let path = directory.path().join(format!("file-{filename}"));
    std::fs::write(&path, &bytes)?;
    let mime_type = if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    let preview = if mime_type == "application/pdf" && data_url.len() <= 512 * 1024 {
        format!("data:application/pdf;base64,{data}")
    } else {
        String::new()
    };
    let name: String = name
        .chars()
        .take(255)
        .filter(|character| !character.is_control())
        .collect();
    Ok(UploadedDocument {
        _directory: directory,
        path,
        name,
        anchor: String::new(),
        preview,
        mime_type,
        upload_id: String::new(),
    })
}
