//! Bounded shell-free subprocess execution for extension scripts.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::ScriptRequest;
use crate::ScriptResponse;

const TERMINATED_IO_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Size limits applied to one script invocation.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputLimits {
    stdin_bytes: usize,
    stdout_bytes: usize,
    stderr_bytes: usize,
}

impl OutputLimits {
    /// Creates explicit byte limits for stdin, stdout, and stderr.
    #[must_use]
    pub fn new(stdin_bytes: usize, stdout_bytes: usize, stderr_bytes: usize) -> Self {
        Self {
            stdin_bytes,
            stdout_bytes,
            stderr_bytes,
        }
    }
}

/// Shell-free script invocation request.
pub struct SubprocessRequest {
    argv: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    limits: OutputLimits,
    timeout: Duration,
    cancellation: CancellationToken,
}

impl std::fmt::Debug for SubprocessRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let environment_names = self
            .environment
            .iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        formatter
            .debug_struct("SubprocessRequest")
            .field("argv", &self.argv)
            .field("environment", &environment_names)
            .field("limits", &self.limits)
            .field("timeout", &self.timeout)
            .field("cancellation", &self.cancellation)
            .finish()
    }
}

impl SubprocessRequest {
    /// Creates a request with an argv vector.
    #[must_use]
    pub fn new(argv: Vec<OsString>) -> Self {
        Self {
            argv,
            environment: Vec::new(),
            limits: OutputLimits::new(64 * 1024, 64 * 1024, 8 * 1024),
            timeout: Duration::from_secs(10),
            cancellation: CancellationToken::new(),
        }
    }

    /// Replaces the byte limits applied to this invocation.
    #[must_use]
    pub fn with_limits(mut self, limits: OutputLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Adds sensitive environment variables for this one child process.
    #[must_use]
    pub fn with_environment(
        mut self,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        self.environment.extend(environment);
        self
    }

    /// Replaces the wall-clock timeout applied to this invocation.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replaces the cancellation token observed by this invocation.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// Captured script output after a successful exit.
#[derive(Debug)]
pub struct SubprocessOutput {
    /// Captured stdout bytes.
    pub stdout: Vec<u8>,
}

/// Execution failure with a prompt-free causal diagnostic.
#[derive(Debug, Error)]
#[error("script execution failed: {kind}")]
pub struct SubprocessFailure {
    /// Causal failure category that never includes stdin or captured output.
    pub kind: SubprocessFailureKind,
    /// Prompt-free details safe for diagnostic logging.
    pub diagnostic: SubprocessDiagnostic,
}

/// Prompt-free details that identify a subprocess failure without exposing its input or output.
#[derive(Debug, Default)]
pub struct SubprocessDiagnostic {
    executable: Option<String>,
    stdout: Option<OutputFingerprint>,
    stderr: Option<StderrFingerprint>,
}

impl SubprocessDiagnostic {
    /// Returns a bounded diagnostic suitable for internal logs.
    #[must_use]
    pub fn summary(&self, kind: &SubprocessFailureKind) -> String {
        let mut details = vec![kind.to_string()];
        if let Some(error) = io_error(kind) {
            details.push(format!(
                "error_kind={:?}, errno={:?}",
                error.kind(),
                error.raw_os_error()
            ));
        }
        if let Some(executable) = &self.executable {
            details.push(format!("executable={executable}"));
        }
        if let Some(stdout) = &self.stdout {
            details.push(format!(
                "stdout_bytes={}, stdout_sha256={}",
                stdout.bytes, stdout.sha256
            ));
        }
        if let Some(stderr) = &self.stderr {
            details.push(format!(
                "stderr_bytes={}, stderr_sha256={}",
                stderr.bytes, stderr.sha256
            ));
        }
        if let SubprocessFailureKind::DeserializeResponse(error) = kind {
            details.push(format!(
                "serde_error={error}, serde_category={:?}, serde_line={}, serde_column={}",
                error.classify(),
                error.line(),
                error.column()
            ));
        }
        details.join("; ")
    }
}

fn io_error(kind: &SubprocessFailureKind) -> Option<&io::Error> {
    match kind {
        SubprocessFailureKind::Spawn(error)
        | SubprocessFailureKind::StdinWrite(error)
        | SubprocessFailureKind::OutputRead(error)
        | SubprocessFailureKind::Wait(error) => Some(error),
        SubprocessFailureKind::EmptyArgv
        | SubprocessFailureKind::StdinLimitExceeded
        | SubprocessFailureKind::SerializeRequest(_)
        | SubprocessFailureKind::DeserializeResponse(_)
        | SubprocessFailureKind::ResponseMismatch
        | SubprocessFailureKind::TimedOut
        | SubprocessFailureKind::Cancelled
        | SubprocessFailureKind::OutputLimitExceeded { .. }
        | SubprocessFailureKind::UnsuccessfulExit { .. }
        | SubprocessFailureKind::TaskJoin => None,
    }
}

#[derive(Debug)]
struct StderrFingerprint {
    bytes: usize,
    sha256: String,
}

#[derive(Debug)]
struct OutputFingerprint {
    bytes: usize,
    sha256: String,
}

/// Categories of script execution failure.
#[derive(Debug)]
pub enum SubprocessFailureKind {
    /// The argv vector contained no program.
    EmptyArgv,
    /// Input exceeded the configured byte limit before process startup.
    StdinLimitExceeded,
    /// The host request could not be serialized as JSON.
    SerializeRequest(serde_json::Error),
    /// The script response was not exactly one valid JSON document.
    DeserializeResponse(serde_json::Error),
    /// The script response did not match the request protocol or identifier.
    ResponseMismatch,
    /// The process could not be started.
    Spawn(io::Error),
    /// Writing stdin failed.
    StdinWrite(io::Error),
    /// Reading one output stream failed.
    OutputRead(io::Error),
    /// Waiting for the child process failed.
    Wait(io::Error),
    /// The child exceeded the configured wall-clock timeout.
    TimedOut,
    /// The supplied cancellation token was cancelled.
    Cancelled,
    /// One stream exceeded its configured byte limit.
    OutputLimitExceeded { stream: OutputStream },
    /// The child exited unsuccessfully.
    UnsuccessfulExit { status: ExitStatus },
    /// An internal I/O task terminated unexpectedly.
    TaskJoin,
}

impl std::fmt::Display for SubprocessFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyArgv => formatter.write_str("script argv is empty"),
            Self::StdinLimitExceeded => formatter.write_str("script stdin exceeds its byte limit"),
            Self::SerializeRequest(_) => formatter.write_str("script request could not be encoded"),
            Self::DeserializeResponse(_) => {
                formatter.write_str("script response was not valid JSON")
            }
            Self::ResponseMismatch => {
                formatter.write_str("script response did not match its request")
            }
            Self::Spawn(_) => formatter.write_str("script could not be started"),
            Self::StdinWrite(_) => formatter.write_str("script stdin could not be written"),
            Self::OutputRead(_) => formatter.write_str("script output could not be read"),
            Self::Wait(_) => formatter.write_str("script process could not be awaited"),
            Self::TimedOut => formatter.write_str("script timed out"),
            Self::Cancelled => formatter.write_str("script was cancelled"),
            Self::OutputLimitExceeded { stream } => {
                write!(formatter, "script {stream} exceeds its byte limit")
            }
            Self::UnsuccessfulExit { status } => {
                write!(formatter, "script exited unsuccessfully ({status})")
            }
            Self::TaskJoin => formatter.write_str("script I/O task ended unexpectedly"),
        }
    }
}

/// Stream whose configured output limit was exceeded.
#[derive(Debug)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl std::fmt::Display for OutputStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdout => formatter.write_str("stdout"),
            Self::Stderr => formatter.write_str("stderr"),
        }
    }
}

/// Failure returned by the subprocess executor.
pub type SubprocessError = SubprocessFailure;

/// Executes one shell-free script process.
pub struct SubprocessExecutor;

impl SubprocessExecutor {
    /// Exchanges one JSON request and response with an argv-defined script.
    ///
    /// # Errors
    ///
    /// Returns a prompt-free causal failure when the process cannot complete.
    pub async fn invoke(
        request: SubprocessRequest,
        protocol_request: &ScriptRequest,
    ) -> Result<ScriptResponse, SubprocessError> {
        let stdin = serde_json::to_vec(protocol_request)
            .map_err(|source| failure(SubprocessFailureKind::SerializeRequest(source)))?;
        let output = Self::run(request, stdin).await?;
        let response =
            serde_json::from_slice::<ScriptResponse>(&output.stdout).map_err(|source| {
                failure_with_stdout(
                    SubprocessFailureKind::DeserializeResponse(source),
                    &output.stdout,
                )
            })?;
        if response.protocol != protocol_request.protocol
            || response.request_id != protocol_request.request_id
        {
            return Err(failure(SubprocessFailureKind::ResponseMismatch));
        }
        Ok(response)
    }

    async fn run(
        request: SubprocessRequest,
        stdin: Vec<u8>,
    ) -> Result<SubprocessOutput, SubprocessError> {
        let SubprocessRequest {
            argv,
            environment,
            limits,
            timeout,
            cancellation,
        } = request;
        if argv.is_empty() {
            return Err(failure(SubprocessFailureKind::EmptyArgv));
        }
        if stdin.len() > limits.stdin_bytes {
            return Err(failure(SubprocessFailureKind::StdinLimitExceeded));
        }

        let deadline = tokio::time::Instant::now() + timeout;
        let executable = executable_name(&argv);
        let SpawnedChild {
            mut child,
            process_group_id,
        } = spawn(&argv, &environment).map_err(|source| {
            failure_with_executable(SubprocessFailureKind::Spawn(source), executable)
        })?;
        let mut process_group_cleanup = ProcessGroupCleanup::new(process_group_id);
        let stdin_task = tokio::spawn(write_stdin(child.stdin.take(), stdin));
        let stdout_task = tokio::spawn(read_bounded(child.stdout.take(), limits.stdout_bytes));
        let stderr_task = tokio::spawn(read_bounded(child.stderr.take(), limits.stderr_bytes));
        let (status, terminated) =
            wait_for_exit(&mut child, process_group_id, &cancellation, deadline).await;
        let outputs = collect_output_tasks(
            stdin_task,
            stdout_task,
            stderr_task,
            process_group_id,
            &cancellation,
            deadline,
            terminated,
        )
        .await;
        let status = status?;
        let (stdout, stderr) = outputs?;
        process_group_cleanup.disarm();

        if stdout.exceeded {
            return Err(failure(SubprocessFailureKind::OutputLimitExceeded {
                stream: OutputStream::Stdout,
            }));
        }
        if stderr.exceeded {
            return Err(failure(SubprocessFailureKind::OutputLimitExceeded {
                stream: OutputStream::Stderr,
            }));
        }
        if !status.success() {
            return Err(failure_with_stderr(
                SubprocessFailureKind::UnsuccessfulExit { status },
                stderr.bytes,
            ));
        }

        Ok(SubprocessOutput {
            stdout: stdout.bytes,
        })
    }
}

async fn collect_output_tasks(
    mut stdin_task: tokio::task::JoinHandle<Result<(), SubprocessFailureKind>>,
    mut stdout_task: tokio::task::JoinHandle<Result<BoundedBytes, SubprocessFailureKind>>,
    mut stderr_task: tokio::task::JoinHandle<Result<BoundedBytes, SubprocessFailureKind>>,
    process_group_id: Option<u32>,
    cancellation: &CancellationToken,
    deadline: tokio::time::Instant,
    terminated: bool,
) -> Result<(BoundedBytes, BoundedBytes), SubprocessError> {
    let join = async {
        let (stdin, stdout, stderr) =
            tokio::join!(&mut stdin_task, &mut stdout_task, &mut stderr_task);
        join_task(stdin)?;
        Ok((join_task(stdout)?, join_task(stderr)?))
    };
    let outcome = if terminated {
        match tokio::time::timeout(TERMINATED_IO_DRAIN_TIMEOUT, join).await {
            Ok(outputs) => outputs,
            Err(_) => {
                stdin_task.abort();
                stdout_task.abort();
                stderr_task.abort();
                Ok((BoundedBytes::default(), BoundedBytes::default()))
            }
        }
    } else {
        tokio::select! {
            outputs = join => outputs,
            _ = cancellation.cancelled() => terminate_after_child_exit(process_group_id, SubprocessFailureKind::Cancelled),
            _ = tokio::time::sleep_until(deadline) => terminate_after_child_exit(process_group_id, SubprocessFailureKind::TimedOut),
        }
    };
    if outcome.is_err() {
        stdin_task.abort();
        stdout_task.abort();
        stderr_task.abort();
    }
    outcome
}

fn join_task<T>(
    task: Result<Result<T, SubprocessFailureKind>, tokio::task::JoinError>,
) -> Result<T, SubprocessError> {
    task.map_err(|_| failure(SubprocessFailureKind::TaskJoin))?
        .map_err(failure)
}

struct SpawnedChild {
    child: Child,
    process_group_id: Option<u32>,
}

struct ProcessGroupCleanup {
    process_group_id: Option<u32>,
    armed: bool,
}

impl ProcessGroupCleanup {
    fn new(process_group_id: Option<u32>) -> Self {
        Self {
            process_group_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = terminate_process_group(self.process_group_id);
        }
    }
}

fn spawn(
    argv: &[OsString],
    environment: &[(OsString, OsString)],
) -> Result<SpawnedChild, io::Error> {
    let (program, args) = argv.split_first().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "script argv must include a program",
        )
    })?;
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(environment.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    let child = command.spawn()?;
    Ok(SpawnedChild {
        process_group_id: child.id(),
        child,
    })
}

fn executable_name(argv: &[OsString]) -> Option<String> {
    argv.first().and_then(|program| {
        Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| {
                !name.is_empty() && name.chars().all(|character| !character.is_control())
            })
            .map(str::to_owned)
    })
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    // SAFETY: pre_exec runs only in the child between fork and exec. The
    // callback calls the async-signal-safe setsid/setpgid wrappers only.
    unsafe {
        command.pre_exec(xedoc_utils_pty::process_group::detach_from_tty);
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

async fn write_stdin(
    stdin: Option<tokio::process::ChildStdin>,
    bytes: Vec<u8>,
) -> Result<(), SubprocessFailureKind> {
    let Some(mut stdin) = stdin else {
        return Ok(());
    };
    stdin
        .write_all(&bytes)
        .await
        .map_err(SubprocessFailureKind::StdinWrite)
}

async fn read_bounded<R>(
    reader: Option<R>,
    limit: usize,
) -> Result<BoundedBytes, SubprocessFailureKind>
where
    R: AsyncRead + Unpin,
{
    let Some(mut reader) = reader else {
        return Ok(BoundedBytes::default());
    };
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8_192];
    let mut exceeded = false;

    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(SubprocessFailureKind::OutputRead)?;
        if read == 0 {
            return Ok(BoundedBytes { bytes, exceeded });
        }
        let remaining = limit.saturating_sub(bytes.len());
        let accepted = read.min(remaining);
        bytes.extend_from_slice(&buffer[..accepted]);
        exceeded |= accepted < read;
    }
}

async fn wait_for_exit(
    child: &mut Child,
    process_group_id: Option<u32>,
    cancellation: &CancellationToken,
    deadline: tokio::time::Instant,
) -> (Result<ExitStatus, SubprocessError>, bool) {
    tokio::select! {
        status = child.wait() => (status.map_err(|source| failure(SubprocessFailureKind::Wait(source))), false),
        _ = cancellation.cancelled() => (terminate(child, process_group_id, SubprocessFailureKind::Cancelled).await, true),
        _ = tokio::time::sleep_until(deadline) => (terminate(child, process_group_id, SubprocessFailureKind::TimedOut).await, true),
    }
}

fn terminate_after_child_exit(
    process_group_id: Option<u32>,
    kind: SubprocessFailureKind,
) -> Result<(BoundedBytes, BoundedBytes), SubprocessError> {
    terminate_process_group(process_group_id)?;
    Err(failure(kind))
}

async fn terminate(
    child: &mut Child,
    process_group_id: Option<u32>,
    kind: SubprocessFailureKind,
) -> Result<ExitStatus, SubprocessError> {
    terminate_process_tree(child, process_group_id)?;
    child
        .wait()
        .await
        .map_err(|source| failure(SubprocessFailureKind::Wait(source)))?;
    Err(failure(kind))
}

fn terminate_process_tree(
    child: &mut Child,
    process_group_id: Option<u32>,
) -> Result<(), SubprocessError> {
    let Some(process_group_id) = process_group_id else {
        return child
            .start_kill()
            .map_err(|source| failure(SubprocessFailureKind::Wait(source)));
    };

    terminate_process_group(Some(process_group_id))
}

fn terminate_process_group(process_group_id: Option<u32>) -> Result<(), SubprocessError> {
    let Some(process_group_id) = process_group_id else {
        return Ok(());
    };

    #[cfg(unix)]
    return xedoc_utils_pty::process_group::kill_process_group(process_group_id)
        .map_err(|source| failure(SubprocessFailureKind::Wait(source)));

    #[cfg(windows)]
    return std::process::Command::new("taskkill")
        .args(["/PID", &process_group_id.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .ok_or_else(|| io::Error::other("taskkill did not terminate script process tree"))
        })
        .map_err(|source| failure(SubprocessFailureKind::Wait(source)));

    #[cfg(not(any(unix, windows)))]
    {
        let _ = process_group_id;
        Ok(())
    }
}

fn failure(kind: SubprocessFailureKind) -> SubprocessFailure {
    SubprocessFailure {
        kind,
        diagnostic: SubprocessDiagnostic::default(),
    }
}

fn failure_with_executable(
    kind: SubprocessFailureKind,
    executable: Option<String>,
) -> SubprocessFailure {
    SubprocessFailure {
        kind,
        diagnostic: SubprocessDiagnostic {
            executable,
            stdout: None,
            stderr: None,
        },
    }
}

fn failure_with_stdout(kind: SubprocessFailureKind, stdout: &[u8]) -> SubprocessFailure {
    SubprocessFailure {
        kind,
        diagnostic: SubprocessDiagnostic {
            executable: None,
            stdout: Some(OutputFingerprint {
                bytes: stdout.len(),
                sha256: format!("{:x}", Sha256::digest(stdout)),
            }),
            stderr: None,
        },
    }
}

fn failure_with_stderr(kind: SubprocessFailureKind, stderr: Vec<u8>) -> SubprocessFailure {
    let stderr = (!stderr.is_empty()).then(|| StderrFingerprint {
        bytes: stderr.len(),
        sha256: format!("{:x}", Sha256::digest(stderr)),
    });
    SubprocessFailure {
        kind,
        diagnostic: SubprocessDiagnostic {
            executable: None,
            stdout: None,
            stderr,
        },
    }
}

#[derive(Default)]
struct BoundedBytes {
    bytes: Vec<u8>,
    exceeded: bool,
}
