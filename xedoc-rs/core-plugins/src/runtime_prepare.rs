use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

const MANIFEST_LIMIT: u64 = 256 * 1024;
const PREPARE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Deserialize)]
struct Manifest {
    gui: Option<Gui>,
}

#[derive(Deserialize)]
struct Gui {
    prepare: Option<Preparation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preparation {
    entrypoint: String,
    interpreter: Option<String>,
}

fn read_preparation(plugin_root: &Path) -> Result<Option<Preparation>, String> {
    let root = plugin_root
        .canonicalize()
        .map_err(|err| format!("cannot resolve plugin preparation root: {err}"))?;
    let manifest_path = root.join(".xedoc-plugin/plugin.json");
    let manifest = match File::open(&manifest_path) {
        Ok(manifest) => manifest,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read plugin preparation manifest: {err}")),
    };
    if !manifest_path
        .canonicalize()
        .map_err(|err| format!("cannot resolve plugin preparation manifest: {err}"))?
        .starts_with(&root)
    {
        return Err("plugin preparation manifest is outside the plugin".to_string());
    }
    let mut contents = Vec::new();
    manifest
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut contents)
        .map_err(|err| format!("cannot read plugin preparation manifest: {err}"))?;
    if contents.len() as u64 > MANIFEST_LIMIT {
        return Err("plugin preparation manifest exceeds 256 KiB".to_string());
    }
    let manifest: Manifest = serde_json::from_slice(&contents)
        .map_err(|err| format!("invalid plugin preparation manifest: {err}"))?;
    Ok(manifest.gui.and_then(|gui| gui.prepare))
}

pub(crate) fn has_runtime_preparation(plugin_root: &Path) -> Result<bool, String> {
    read_preparation(plugin_root).map(|preparation| preparation.is_some())
}

pub(crate) fn prepare_plugin_runtime(plugin_root: &Path) -> Result<(), String> {
    let Some(preparation) = read_preparation(plugin_root)? else {
        return Ok(());
    };
    let root = plugin_root
        .canonicalize()
        .map_err(|err| format!("cannot resolve plugin preparation root: {err}"))?;
    if !preparation.entrypoint.starts_with("./") {
        return Err("gui.prepare.entrypoint must start with ./".to_string());
    }
    let executable = root
        .join(&preparation.entrypoint)
        .canonicalize()
        .map_err(|err| format!("cannot resolve gui.prepare.entrypoint: {err}"))?;
    if !executable.starts_with(&root) || !executable.is_file() {
        return Err("gui.prepare.entrypoint must be a file inside the plugin".to_string());
    }
    let mut command = match preparation.interpreter.as_deref() {
        Some("node") => {
            let mut command = Command::new("node");
            command.arg(&executable);
            command
        }
        None => Command::new(&executable),
        Some(_) => return Err("gui.prepare.interpreter must be node or omitted".to_string()),
    };
    let retry = format!(
        "retry plugin installation, or run {:?} from {}{}",
        executable,
        root.display(),
        if preparation.interpreter.is_some() {
            " with node"
        } else {
            ""
        }
    );
    command
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("plugin runtime preparation failed: {err}; {retry}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "plugin runtime preparation exited with {status}; {retry}"
                ));
            }
            Ok(None) => {}
            Err(err) => {
                stop_preparation(&mut child);
                return Err(format!(
                    "cannot wait for plugin runtime preparation: {err}; {retry}"
                ));
            }
        }
        if start.elapsed() >= PREPARE_TIMEOUT {
            stop_preparation(&mut child);
            return Err(format!(
                "plugin runtime preparation timed out after 180s; {retry}"
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop_preparation(child: &mut Child) {
    #[cfg(unix)]
    let _ = Command::new("/bin/kill")
        .args(["-KILL", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}
