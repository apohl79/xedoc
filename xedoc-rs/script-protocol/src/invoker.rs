//! Bounded one-shot invocation for Xedoc extension scripts.

use std::ffi::OsString;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::OutputLimits;
use crate::ResponseOutcome;
use crate::ScriptRequest;
use crate::ScriptResult;
use crate::SubprocessError;
use crate::SubprocessExecutor;
use crate::SubprocessRequest;

const MAX_SCRIPT_STDIN_BYTES: usize = 1_048_576;
const MAX_SCRIPT_STDOUT_BYTES: usize = 262_144;
const MAX_SCRIPT_STDERR_BYTES: usize = 16_384;

/// Maximum UTF-8 byte length retained for a script result summary.
pub const MAX_SCRIPT_SUMMARY_BYTES: usize = 512;

/// Executes one bounded shell-free extension script request.
pub struct ScriptInvoker {
    argv: Vec<OsString>,
}

impl ScriptInvoker {
    /// Creates a one-shot invoker for an argv-defined script.
    #[must_use]
    pub fn new(argv: Vec<OsString>) -> Self {
        Self { argv }
    }

    /// Exchanges one bounded protocol request and response.
    ///
    /// # Errors
    ///
    /// Returns a prompt-free causal failure when the process cannot complete.
    pub async fn invoke(
        self,
        protocol_request: &ScriptRequest,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Result<ResponseOutcome, SubprocessError> {
        let subprocess_request = SubprocessRequest::new(self.argv)
            .with_limits(OutputLimits::new(
                MAX_SCRIPT_STDIN_BYTES,
                MAX_SCRIPT_STDOUT_BYTES,
                MAX_SCRIPT_STDERR_BYTES,
            ))
            .with_timeout(timeout)
            .with_cancellation(cancellation);
        let mut outcome = SubprocessExecutor::invoke(subprocess_request, protocol_request)
            .await?
            .outcome;
        bound_complete_summary(&mut outcome);
        Ok(outcome)
    }
}

fn bound_complete_summary(outcome: &mut ResponseOutcome) {
    if let ResponseOutcome::Result {
        result: ScriptResult::Complete {
            summary: Some(summary),
        },
    } = outcome
    {
        truncate_utf8(summary, MAX_SCRIPT_SUMMARY_BYTES);
    }
}

fn truncate_utf8(value: &mut String, maximum_bytes: usize) {
    if value.len() <= maximum_bytes {
        return;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}
