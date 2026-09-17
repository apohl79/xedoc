use crate::transport::SessionScriptConnectionScope;
use crate::transport::TransportEvent;
use crate::transport::start_session_script_connection;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Error as IoError;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing::warn;
use xedoc_config::SessionScriptConfigToml;
use xedoc_protocol::ThreadId;

const MAX_SESSION_SCRIPT_STDERR_BYTES: usize = 16 * 1024;

/// Starts configured session scripts as thread-bound, host-authorized children.
///
/// Each child owns a dedicated JSON-RPC pipe connection. Its registration
/// identity comes from an immutable transport scope rather than client input.
pub(crate) struct SessionScriptHost {
    configured_scripts: Vec<SessionScriptConfigToml>,
    transport_event_tx: mpsc::Sender<TransportEvent>,
    scripts_by_thread: HashMap<ThreadId, Vec<HostedSessionScript>>,
    extension_scripts_by_thread: HashMap<ThreadId, HashMap<String, HostedSessionScript>>,
}

struct HostedSessionScript {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl SessionScriptHost {
    pub(crate) fn new(
        configured_scripts: Vec<SessionScriptConfigToml>,
        transport_event_tx: mpsc::Sender<TransportEvent>,
    ) -> Self {
        let configured_script_id_counts = configured_scripts
            .iter()
            .filter(|script| !script.id.trim().is_empty() && script.command_argv().is_some())
            .fold(HashMap::<String, usize>::new(), |mut counts, script| {
                *counts.entry(script.id.clone()).or_default() += 1;
                counts
            });
        let configured_scripts = configured_scripts
            .into_iter()
            .filter(|script| configured_script_id_counts.get(script.id.as_str()) == Some(&1))
            .collect();
        Self {
            configured_scripts,
            transport_event_tx,
            scripts_by_thread: HashMap::new(),
            extension_scripts_by_thread: HashMap::new(),
        }
    }

    pub(crate) async fn start_for_thread(&mut self, thread_id: ThreadId) {
        if self.scripts_by_thread.contains_key(&thread_id) {
            return;
        }

        let mut hosted_scripts = Vec::new();
        for script in &self.configured_scripts {
            let argv = script
                .command_argv()
                .unwrap_or_default()
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            match launch_session_script(
                &script.id,
                &argv,
                thread_id,
                self.transport_event_tx.clone(),
            )
            .await
            {
                Ok(hosted_script) => hosted_scripts.push(hosted_script),
                Err(error) => {
                    warn!(
                        script_id = script.id,
                        thread_id = %thread_id,
                        error = %error,
                        "failed to launch configured session script"
                    );
                }
            }
        }
        info!(
            thread_id = %thread_id,
            launched_count = hosted_scripts.len(),
            "started configured session scripts for loaded root thread"
        );
        self.scripts_by_thread.insert(thread_id, hosted_scripts);
    }

    pub(crate) async fn start_extension(
        &mut self,
        thread_id: ThreadId,
        extension_id: String,
        entrypoint: PathBuf,
    ) -> Result<(), IoError> {
        if self
            .extension_scripts_by_thread
            .get(&thread_id)
            .is_some_and(|extensions| extensions.contains_key(&extension_id))
        {
            return Ok(());
        }
        let argv = [OsString::from(entrypoint.as_os_str())];
        let hosted_script = launch_session_script(
            &extension_id,
            &argv,
            thread_id,
            self.transport_event_tx.clone(),
        )
        .await?;
        self.extension_scripts_by_thread
            .entry(thread_id)
            .or_default()
            .insert(extension_id, hosted_script);
        Ok(())
    }

    pub(crate) async fn stop_extension(&mut self, thread_id: ThreadId, extension_id: &str) {
        let Some(hosted_script) = self
            .extension_scripts_by_thread
            .get_mut(&thread_id)
            .and_then(|extensions| extensions.remove(extension_id))
        else {
            return;
        };
        hosted_script.cancellation.cancel();
        let _ = hosted_script.task.await;
        if self
            .extension_scripts_by_thread
            .get(&thread_id)
            .is_some_and(HashMap::is_empty)
        {
            self.extension_scripts_by_thread.remove(&thread_id);
        }
    }

    pub(crate) async fn stop_thread(&mut self, thread_id: ThreadId) {
        let hosted_scripts = self
            .scripts_by_thread
            .remove(&thread_id)
            .unwrap_or_default();
        let extension_scripts = self
            .extension_scripts_by_thread
            .remove(&thread_id)
            .unwrap_or_default()
            .into_values()
            .collect::<Vec<_>>();
        for hosted_script in hosted_scripts.iter().chain(&extension_scripts) {
            hosted_script.cancellation.cancel();
        }
        for hosted_script in hosted_scripts.into_iter().chain(extension_scripts) {
            let _ = hosted_script.task.await;
        }
    }

    pub(crate) async fn shutdown(&mut self) {
        let thread_ids = self
            .scripts_by_thread
            .keys()
            .chain(self.extension_scripts_by_thread.keys())
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for thread_id in thread_ids {
            self.stop_thread(thread_id).await;
        }
    }
}

async fn launch_session_script(
    script_id: &str,
    argv: &[OsString],
    thread_id: ThreadId,
    transport_event_tx: mpsc::Sender<TransportEvent>,
) -> Result<HostedSessionScript, IoError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(IoError::other("session script command is empty"));
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .env("XEDOC_SESSION_SCRIPT_ID", script_id)
        .env("XEDOC_SESSION_SCRIPT_THREAD_ID", thread_id.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    info!(script_id, thread_id = %thread_id, "started session script child");
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| IoError::other("session script stdout pipe was unavailable"))?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| IoError::other("session script stdin pipe was unavailable"))?;
    let child_stderr = child.stderr.take();
    let cancellation = CancellationToken::new();
    let (_, connection_task) = match start_session_script_connection(
        child_stdout,
        child_stdin,
        transport_event_tx,
        SessionScriptConnectionScope::new(script_id, thread_id.to_string()),
        cancellation.clone(),
    )
    .await
    {
        Ok(connection) => connection,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
    };
    let script_id = script_id.to_string();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        let stderr_task = child_stderr.map(|stderr| {
            tokio::spawn(drain_session_script_stderr(
                stderr,
                script_id.clone(),
                thread_id,
            ))
        });
        let status = tokio::select! {
            status = child.wait() => status,
            _ = task_cancellation.cancelled() => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        match status {
            Ok(status) if status.success() => {
                info!(script_id, thread_id = %thread_id, "session script exited");
            }
            Ok(status) => {
                warn!(script_id, thread_id = %thread_id, ?status, "session script exited unsuccessfully");
            }
            Err(error) => {
                warn!(script_id, thread_id = %thread_id, %error, "failed waiting for session script");
            }
        }
        task_cancellation.cancel();
        let _ = connection_task.await;
        if let Some(stderr_task) = stderr_task {
            stderr_task.abort();
            let _ = stderr_task.await;
        }
    });

    Ok(HostedSessionScript { cancellation, task })
}

async fn drain_session_script_stderr(
    mut stderr: tokio::process::ChildStderr,
    script_id: String,
    thread_id: ThreadId,
) {
    let mut buffer = [0_u8; 1024];
    let mut bytes_read = 0_usize;
    loop {
        let read = match stderr.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                warn!(script_id, thread_id = %thread_id, %error, "failed reading session script stderr");
                return;
            }
        };
        if read == 0 {
            return;
        }
        bytes_read = bytes_read.saturating_add(read);
        if bytes_read > MAX_SESSION_SCRIPT_STDERR_BYTES {
            warn!(
                script_id,
                thread_id = %thread_id,
                max_bytes = MAX_SESSION_SCRIPT_STDERR_BYTES,
                "session script stderr exceeded the diagnostic output limit"
            );
            return;
        }
    }
}
