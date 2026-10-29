//! Collects markdown stream source at newline boundaries.
//!
//! `MarkdownStreamCollector` buffers incoming token deltas and exposes a commit boundary at each
//! newline. The stream controllers (`streaming/controller.rs`) call `commit_complete_source()`
//! after each newline-bearing delta to obtain the completed prefix for re-rendering, leaving the
//! trailing incomplete line in the buffer for the next delta.
//!
//! On finalization, `finalize_and_take_source()` transfers the complete source buffer to the
//! controller (including a final line that may lack a trailing newline).

#![allow(clippy::disallowed_methods)]

use std::path::Path;

/// Newline-gated accumulator that buffers raw markdown source and commits only completed lines.
///
/// The buffer tracks how many source bytes have already been committed via
/// `committed_source_len`, so each `commit_complete_source()` call returns the range of the newly
/// completed portion. This design lets the stream controller re-render the entire accumulated
/// source while only appending new content.
///
/// The collector does not parse markdown in production. It only defines stable source boundaries;
/// rendering lives in the stream controllers so width changes can re-render from one accumulated
/// source string.
pub struct MarkdownStreamCollector {
    buffer: String,
    committed_source_len: usize,
    width: Option<usize>,
}

impl MarkdownStreamCollector {
    /// Create a collector that accumulates raw markdown deltas.
    ///
    /// `width` and `cwd` are only used by test-only rendering helpers; production stream commits
    /// operate on raw source boundaries. The collector snapshots `cwd` so test rendering keeps
    /// local file-link display stable across incremental commits.
    pub fn new(width: Option<usize>, cwd: &Path) -> Self {
        #[cfg(not(test))]
        let _ = cwd;

        Self {
            buffer: String::new(),
            committed_source_len: 0,
            width,
        }
    }

    /// Update the rendering width used by test-only line-commit helpers.
    pub fn set_width(&mut self, width: Option<usize>) {
        self.width = width;
    }

    /// Reset all buffered source and commit bookkeeping.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.committed_source_len = 0;
    }

    /// Append a raw streaming delta to the internal source buffer.
    pub fn push_delta(&mut self, delta: &str) {
        tracing::trace!("push_delta: {delta:?}");
        self.buffer.push_str(delta);
    }

    /// Commit newly completed raw markdown source up to the last newline.
    ///
    /// This returns the range of source that has not been returned by a previous commit. Calling it
    /// after a delta without a newline returns `None`, which prevents the live stream from rendering
    /// incomplete markdown blocks that may change meaning when the rest of the line arrives.
    pub fn commit_complete_source(&mut self) -> Option<std::ops::Range<usize>> {
        let commit_end = self.buffer.rfind('\n').map(|idx| idx + 1)?;
        let commit_start = self.committed_source_len;
        if commit_end <= commit_start {
            return None;
        }

        self.committed_source_len = commit_end;
        Some(commit_start..commit_end)
    }

    /// Return the newline-terminated source that is safe to render.
    pub fn committed_source(&self) -> &str {
        &self.buffer[..self.committed_source_len]
    }

    /// Return the complete source accumulated for the current stream.
    pub fn source(&self) -> &str {
        &self.buffer
    }

    /// Finalize the stream and transfer its complete raw source.
    ///
    /// Ensures the returned source chunk is newline-terminated when non-empty so callers can
    /// safely run markdown block parsing on the final chunk. This method clears the collector;
    /// callers should not invoke it until the stream is truly complete or interrupted output is
    /// being intentionally consolidated.
    pub fn finalize_and_take_source(&mut self) -> String {
        let mut out = std::mem::take(&mut self.buffer);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        self.clear();
        out
    }
}
