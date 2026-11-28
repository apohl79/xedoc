use super::*;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value as JsonValue;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::ChildStdin;
use std::process::ChildStdout;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::TryLockError;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;
use xedoc_app_server_protocol::ThreadVoiceStatusParams;
use xedoc_app_server_protocol::ThreadVoiceStatusResponse;
use xedoc_app_server_protocol::ThreadVoiceSynthesizeParams;
use xedoc_app_server_protocol::ThreadVoiceSynthesizeResponse;
use xedoc_app_server_protocol::ThreadVoiceTranscribeParams;
use xedoc_app_server_protocol::ThreadVoiceTranscribeResponse;
use xedoc_install_context::InstallContext;
use xedoc_login::XedocAuth;

const MAX_INPUT_AUDIO_BYTES: usize = 1_000_000;
const MAX_TTS_TEXT_BYTES: usize = 1200;
const MAX_WORKER_REQUEST_BYTES: usize = 1_500_000;
const MAX_WORKER_RESPONSE_BYTES: usize = 4_000_000;
const VOICE_WORKER_OPERATION_TIMEOUT: Duration = Duration::from_secs(240);
const VOICE_WORKER_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone)]
pub(crate) struct VoiceRequestProcessor {
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    config: Arc<Config>,
    runtime: Result<VoiceRuntime, String>,
    worker: Arc<Mutex<Option<VoiceWorker>>>,
}

#[derive(Clone)]
struct VoiceRuntime {
    python: PathBuf,
    site_packages: PathBuf,
}

struct VoiceWorker {
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl VoiceRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            auth_manager,
            thread_manager,
            config,
            runtime: voice_runtime(),
            worker: Arc::new(Mutex::new(None)),
        }
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn status(
        &self,
        params: ThreadVoiceStatusParams,
    ) -> Result<ThreadVoiceStatusResponse, JSONRPCErrorError> {
        self.ensure_thread(&params.thread_id).await?;
        let auth = self.auth_manager.auth().await;
        let unavailable_reason = if !auth.as_ref().is_some_and(openai_auth_is_configured) {
            Some("Configure OpenAI OAuth or an API key on the host to use voice.".to_string())
        } else if let Err(reason) = &self.runtime {
            Some(reason.clone())
        } else {
            self.worker_request(serde_json::json!({"action": "status"}))
                .await
                .err()
                .map(|error| {
                    format!(
                        "The host speech runtime could not start: {}. Reinstall or update Xedoc on the host.",
                        error.message
                    )
                })
        };
        Ok(ThreadVoiceStatusResponse {
            available: unavailable_reason.is_none(),
            unavailable_reason,
        })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn transcribe(
        &self,
        params: ThreadVoiceTranscribeParams,
    ) -> Result<ThreadVoiceTranscribeResponse, JSONRPCErrorError> {
        self.require_voice_available(&params.thread_id).await?;
        if params.audio_base64.len() > MAX_INPUT_AUDIO_BYTES.div_ceil(3) * 4 {
            return Err(invalid_request("voice recording is too large"));
        }
        let audio = STANDARD
            .decode(&params.audio_base64)
            .map_err(|_| invalid_request("voice recording is not valid base64"))?;
        if audio.is_empty() || audio.len() > MAX_INPUT_AUDIO_BYTES {
            return Err(invalid_request("voice recording size is invalid"));
        }
        let response = self
            .worker_request(serde_json::json!({
                "action": "transcribe",
                "audioBase64": params.audio_base64,
                "locale": normalize_locale(&params.locale),
            }))
            .await?;
        let text = response
            .get("text")
            .and_then(JsonValue::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| internal_error("local speech recognition returned no text"))?;
        Ok(ThreadVoiceTranscribeResponse {
            text: text.to_string(),
        })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn synthesize(
        &self,
        params: ThreadVoiceSynthesizeParams,
    ) -> Result<ThreadVoiceSynthesizeResponse, JSONRPCErrorError> {
        self.require_voice_available(&params.thread_id).await?;
        if params.text.trim().is_empty() || params.text.len() > MAX_TTS_TEXT_BYTES {
            return Err(invalid_request(
                "speech text must contain 1–1200 UTF-8 bytes",
            ));
        }
        let response = self
            .worker_request(serde_json::json!({
                "action": "synthesize",
                "text": params.text,
                "locale": normalize_locale(&params.locale),
            }))
            .await?;
        let audio_base64 = response
            .get("audioBase64")
            .and_then(JsonValue::as_str)
            .filter(|audio| !audio.is_empty())
            .ok_or_else(|| internal_error("local speech synthesis returned no audio"))?;
        Ok(ThreadVoiceSynthesizeResponse {
            audio_base64: audio_base64.to_string(),
        })
    }

    async fn require_voice_available(&self, thread_id: &str) -> Result<(), JSONRPCErrorError> {
        let status = self
            .status(ThreadVoiceStatusParams {
                thread_id: thread_id.to_string(),
            })
            .await?;
        if let Some(reason) = status.unavailable_reason {
            return Err(invalid_request(reason));
        }
        Ok(())
    }

    async fn ensure_thread(&self, thread_id: &str) -> Result<(), JSONRPCErrorError> {
        let thread_id =
            ThreadId::from_string(thread_id).map_err(|_| invalid_request("invalid thread id"))?;
        self.thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request("thread not found"))?;
        Ok(())
    }

    async fn worker_request(&self, request: JsonValue) -> Result<JsonValue, JSONRPCErrorError> {
        let runtime = self.runtime.clone().map_err(invalid_request)?;
        let xedoc_home = self.config.xedoc_home.to_path_buf();
        let worker_state = Arc::clone(&self.worker);
        let response = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + VOICE_WORKER_OPERATION_TIMEOUT;
            let mut worker = loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("timed out waiting for the local speech worker".to_string());
                }
                match worker_state.try_lock() {
                    Ok(worker) => break worker,
                    Err(TryLockError::WouldBlock) => {
                        std::thread::sleep(remaining.min(VOICE_WORKER_LOCK_POLL_INTERVAL));
                    }
                    Err(TryLockError::Poisoned(_)) => {
                        return Err("local speech worker lock was poisoned".to_string());
                    }
                }
            };
            if worker.as_mut().is_some_and(|worker| {
                let mut child = worker
                    .child
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                !matches!(child.try_wait(), Ok(None))
            }) {
                *worker = None;
            }
            if worker.is_none() {
                *worker = Some(VoiceWorker::start(&runtime, &xedoc_home)?);
            }
            let active_worker = worker
                .as_mut()
                .ok_or_else(|| "local speech worker could not be started".to_string())?;
            let child = Arc::clone(&active_worker.child);
            let (finished_sender, finished_receiver) = mpsc::channel();
            let deadline_monitor = std::thread::Builder::new()
                .name("xedoc-voice-deadline".to_string())
                .spawn(move || {
                    if matches!(
                        finished_receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        terminate_voice_worker(&child);
                        true
                    } else {
                        false
                    }
                })
                .map_err(|error| {
                    format!("could not start local speech deadline monitor: {error}")
                })?;
            let response = active_worker.request(&request);
            let _ = finished_sender.send(());
            let timed_out = deadline_monitor
                .join()
                .map_err(|_| "local speech deadline monitor failed".to_string())?;
            if timed_out {
                *worker = None;
                Err("local speech worker operation exceeded its 240-second deadline".to_string())
            } else {
                response
            }
        })
        .await
        .map_err(|_| internal_error("local speech worker task failed"))?
        .map_err(internal_error)?;
        if response.get("ok").and_then(JsonValue::as_bool) != Some(true) {
            let message = response
                .get("error")
                .and_then(JsonValue::as_str)
                .unwrap_or("local speech processing failed");
            return Err(internal_error(message.to_string()));
        }
        Ok(response)
    }
}

impl VoiceWorker {
    fn start(runtime: &VoiceRuntime, xedoc_home: &Path) -> Result<Self, String> {
        let mut child = Command::new(&runtime.python)
            .arg("-s")
            .arg("-B")
            .arg("-c")
            .arg(include_str!("voice_worker.py"))
            .env("XEDOC_HOME", xedoc_home)
            .env("PYTHONPATH", &runtime.site_packages)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start local speech runtime: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "local speech worker input is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "local speech worker output is unavailable".to_string())?;
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn request(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        let mut encoded = serde_json::to_vec(request)
            .map_err(|error| format!("could not encode local speech request: {error}"))?;
        if encoded.len() > MAX_WORKER_REQUEST_BYTES {
            return Err("local speech request exceeds the size limit".to_string());
        }
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("could not send request to local speech worker: {error}"))?;

        let mut response = Vec::new();
        loop {
            let available = self
                .stdout
                .fill_buf()
                .map_err(|error| format!("could not read local speech worker response: {error}"))?;
            if available.is_empty() {
                break;
            }
            let count = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if response.len().saturating_add(count) > MAX_WORKER_RESPONSE_BYTES {
                return Err("local speech worker response exceeds the size limit".to_string());
            }
            let complete = available[count - 1] == b'\n';
            response.extend_from_slice(&available[..count]);
            self.stdout.consume(count);
            if complete {
                break;
            }
        }
        if response.last() == Some(&b'\n') {
            response.pop();
        }
        if response.is_empty() || response.len() > MAX_WORKER_RESPONSE_BYTES {
            return Err("local speech worker response is empty or too large".to_string());
        }
        serde_json::from_slice(&response)
            .map_err(|error| format!("local speech worker returned invalid JSON: {error}"))
    }
}

impl Drop for VoiceWorker {
    fn drop(&mut self) {
        terminate_voice_worker(&self.child);
    }
}

fn terminate_voice_worker(child: &Mutex<Child>) {
    let mut child = child
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn voice_runtime() -> Result<VoiceRuntime, String> {
    let install = InstallContext::current();
    let missing_runtime =
        "The host speech runtime is missing or incomplete. Reinstall or update Xedoc on the host.";
    let manifest = install
        .bundled_resource("model-router/runtime/model-router-runtime.json")
        .ok_or_else(|| missing_runtime.to_string())?
        .into_path_buf();
    let root = manifest
        .parent()
        .ok_or_else(|| missing_runtime.to_string())?;
    let python = if cfg!(windows) {
        root.join("python").join("python.exe")
    } else {
        install
            .bundled_resource("remote-agent/runtime/python/bin/python3")
            .map_or_else(
                || root.join("python").join("bin").join("python3"),
                PathBuf::from,
            )
    };
    let site_packages = root.join("site-packages");
    if !python.is_file() || !site_packages.join("sherpa_onnx/__init__.py").is_file() {
        return Err(missing_runtime.to_string());
    }
    Ok(VoiceRuntime {
        python,
        site_packages,
    })
}

fn openai_auth_is_configured(auth: &XedocAuth) -> bool {
    auth.api_key().is_some() || auth.is_chatgpt_auth() || auth.is_external_chatgpt_tokens()
}

fn normalize_locale(locale: &str) -> &'static str {
    if locale.to_ascii_lowercase().starts_with("de") {
        "de"
    } else {
        "en"
    }
}
