use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

/// Atomically persist an original output under a content-addressed path.
pub fn spill_original(root: &Path, call_id: &str, text: &str) -> std::io::Result<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let safe_call_id = call_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let dir = root.join(safe_call_id);
    fs::create_dir_all(&dir)?;
    cleanup_orphan_temps(&dir)?;
    let destination = dir.join(format!("{digest}.txt"));
    if destination.exists() {
        return Ok(destination);
    }
    let temporary = dir.join(format!(".{digest}.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&temporary, &destination) {
        Ok(()) => {
            if let Ok(directory) = File::open(&dir) {
                let _ = directory.sync_all();
            }
            Ok(destination)
        }
        Err(_error) if destination.exists() => {
            let _ = fs::remove_file(&temporary);
            Ok(destination)
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

pub(crate) fn decode_call_id(encoded: &str) -> Option<String> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Remove spill files older than `retention_days` and trim newest files to `max_mib`.
pub fn prune_spill_root(root: &Path, retention_days: u32, max_mib: u32) -> std::io::Result<()> {
    let now = std::time::SystemTime::now();
    let max_age = std::time::Duration::from_secs(u64::from(retention_days) * 86_400);
    let mut files = Vec::new();
    for call_dir in fs::read_dir(root).into_iter().flatten().flatten() {
        if !call_dir.path().is_dir() {
            continue;
        }
        for entry in fs::read_dir(call_dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "txt") {
                let modified = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(now);
                if now.duration_since(modified).unwrap_or_default() > max_age {
                    let _ = fs::remove_file(&path);
                } else if let Ok(meta) = entry.metadata() {
                    files.push((modified, meta.len(), path));
                }
            }
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut total = files.iter().map(|(_, size, _)| *size).sum::<u64>();
    let limit = u64::from(max_mib) * 1024 * 1024;
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        let _ = fs::remove_file(path);
        total = total.saturating_sub(size);
    }
    Ok(())
}

fn cleanup_orphan_temps(dir: &Path) -> std::io::Result<()> {
    let now = std::time::SystemTime::now();
    let stale_after = std::time::Duration::from_secs(60 * 60);
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".tmp-"))
        {
            let stale = fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > stale_after);
            if stale {
                let _ = fs::remove_file(path);
            }
        }
    }
    Ok(())
}
