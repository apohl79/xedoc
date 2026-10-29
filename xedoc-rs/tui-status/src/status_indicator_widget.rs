//! A live task status row rendered above the composer while the agent is busy.
//!
//! The row owns spinner timing, the optional interrupt hint, and short inline
//! context (for example, the unified-exec background-process summary). Keeping
//! these pieces on one line avoids vertical layout churn in the bottom pane.

use std::time::Duration;
use std::time::Instant;

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use unicode_width::UnicodeWidthStr;

use crate::app_event_sender::AppEventSender;
use crate::city_lights::CityLightsStylize;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::motion::shimmer_text;
use crate::render::renderable::Renderable;
use crate::text_formatting::capitalize_first;
use crate::tui::FrameRequester;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

pub const STATUS_DETAILS_DEFAULT_MAX_LINES: usize = 3;
const DETAILS_PREFIX: &str = "  └ ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusDetailsCapitalization {
    CapitalizeFirst,
    Preserve,
}

/// Displays a single-line in-progress status with optional wrapped details.
pub struct StatusIndicatorWidget {
    /// Animated header text (defaults to "Working").
    header: String,
    details: Option<String>,
    details_max_lines: usize,
    /// Optional suffix rendered after the elapsed/interrupt segment.
    inline_message: Option<String>,
    show_interrupt_hint: bool,
    interrupt_binding: Option<KeyBinding>,
    show_detach_hint: bool,

    elapsed_running: Duration,
    last_resume_at: Instant,
    is_paused: bool,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    animations_enabled: bool,
}

pub use crate::text_formatting::fmt_elapsed_compact;

impl StatusIndicatorWidget {
    pub fn new(
        app_event_tx: AppEventSender,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
            inline_message: None,
            show_interrupt_hint: true,
            interrupt_binding: Some(key_hint::plain(KeyCode::Esc)),
            show_detach_hint: false,
            elapsed_running: Duration::ZERO,
            last_resume_at: Instant::now(),
            is_paused: false,

            app_event_tx,
            frame_requester,
            animations_enabled,
        }
    }

    pub fn interrupt(&self) {
        self.app_event_tx.interrupt();
    }

    /// Update the animated header label (left of the brackets).
    pub fn update_header(&mut self, header: String) {
        self.header = header;
    }

    /// Update the details text shown below the header.
    pub fn update_details(
        &mut self,
        details: Option<String>,
        capitalization: StatusDetailsCapitalization,
        max_lines: usize,
    ) {
        self.details_max_lines = max_lines.max(1);
        self.details = details
            .filter(|details| !details.is_empty())
            .map(|details| {
                let trimmed = details.trim_start();
                match capitalization {
                    StatusDetailsCapitalization::CapitalizeFirst => capitalize_first(trimmed),
                    StatusDetailsCapitalization::Preserve => trimmed.to_string(),
                }
            });
    }

    /// Update the inline suffix text shown after the elapsed/interrupt hint.
    ///
    /// Callers should provide plain, already-contextualized text. Passing
    /// verbose status prose here can cause frequent width truncation and hide
    /// the more important elapsed/interrupt hint.
    pub fn update_inline_message(&mut self, message: Option<String>) {
        self.inline_message = message
            .map(|message| message.trim().to_string())
            .filter(|message| !message.is_empty());
    }

    pub fn header(&self) -> &str {
        &self.header
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }

    pub fn set_interrupt_hint_visible(&mut self, visible: bool) {
        self.show_interrupt_hint = visible;
    }

    pub fn set_interrupt_binding(&mut self, binding: Option<KeyBinding>) {
        self.interrupt_binding = binding;
    }

    pub fn set_detach_hint_visible(&mut self, visible: bool) {
        self.show_detach_hint = visible;
    }

    pub fn pause_timer(&mut self) {
        self.pause_timer_at(Instant::now());
    }

    pub fn resume_timer(&mut self) {
        self.resume_timer_at(Instant::now());
    }

    pub fn pause_timer_at(&mut self, now: Instant) {
        if self.is_paused {
            return;
        }
        self.elapsed_running += now.saturating_duration_since(self.last_resume_at);
        self.is_paused = true;
    }

    pub fn resume_timer_at(&mut self, now: Instant) {
        if !self.is_paused {
            return;
        }
        self.last_resume_at = now;
        self.is_paused = false;
        self.frame_requester.schedule_frame();
    }

    fn elapsed_duration_at(&self, now: Instant) -> Duration {
        let mut elapsed = self.elapsed_running;
        if !self.is_paused {
            elapsed += now.saturating_duration_since(self.last_resume_at);
        }
        elapsed
    }

    fn elapsed_seconds_at(&self, now: Instant) -> u64 {
        self.elapsed_duration_at(now).as_secs()
    }

    pub fn elapsed_seconds(&self) -> u64 {
        self.elapsed_seconds_at(Instant::now())
    }

    /// Wrap the details text into a fixed width and return the lines, truncating if necessary.
    fn wrapped_details_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(details) = self.details.as_deref() else {
            return Vec::new();
        };
        if width == 0 {
            return Vec::new();
        }

        let prefix_width = UnicodeWidthStr::width(DETAILS_PREFIX);
        let opts = RtOptions::new(usize::from(width))
            .initial_indent(Line::from(DETAILS_PREFIX.dim()))
            .subsequent_indent(Line::from(Span::from(" ".repeat(prefix_width)).dim()))
            .break_words(/*break_words*/ true);

        let mut out = word_wrap_lines(details.lines().map(|line| vec![line.dim()]), opts);

        if out.len() > self.details_max_lines {
            out.truncate(self.details_max_lines);
            let content_width = usize::from(width).saturating_sub(prefix_width).max(1);
            let max_base_len = content_width.saturating_sub(1);
            if let Some(last) = out.last_mut()
                && let Some(span) = last.spans.last_mut()
            {
                let trimmed: String = span.content.as_ref().chars().take(max_base_len).collect();
                *span = format!("{trimmed}…").dim();
            }
        }

        out
    }
}

impl Renderable for StatusIndicatorWidget {
    fn desired_height(&self, width: u16) -> u16 {
        1 + u16::try_from(self.wrapped_details_lines(width).len()).unwrap_or(0)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        if self.animations_enabled {
            // Schedule next animation frame.
            self.frame_requester
                .schedule_frame_in(Duration::from_millis(32));
        }
        let now = Instant::now();
        let elapsed_duration = self.elapsed_duration_at(now);
        let pretty_elapsed = fmt_elapsed_compact(elapsed_duration.as_secs());
        let motion_mode = MotionMode::from_animations_enabled(self.animations_enabled);

        let mut spans = Vec::with_capacity(8);
        if let Some(indicator) = activity_indicator(
            Some(self.last_resume_at),
            motion_mode,
            ReducedMotionIndicator::Hidden,
        ) {
            spans.push(indicator);
            spans.push(" ".into());
        }
        if let Some(part) = compaction_part(&self.header) {
            spans.extend(shimmer_text("Compacting", motion_mode));
            spans.push(" ".into());
            spans.push(format!("part {part}").cl_cyan());
        } else if let Some(stage) = compaction_stage(&self.header) {
            spans.extend(shimmer_text("Compacting", motion_mode));
            spans.push(" ".into());
            spans.push(stage.cl_cyan());
        } else {
            spans.extend(shimmer_text(&self.header, motion_mode));
        }
        if !spans.is_empty() {
            spans.push(" ".into());
        }
        spans.push(format!("({pretty_elapsed}").dim());
        if self.show_interrupt_hint
            && let Some(interrupt_binding) = self.interrupt_binding
        {
            spans.extend(vec![
                " • ".dim(),
                interrupt_binding.into(),
                " to interrupt".dim(),
            ]);
        }
        if self.show_detach_hint {
            spans.extend(vec![" • ".dim(), "ctrl+d".dim(), " to detach".dim()]);
        }
        spans.push(")".dim());
        if let Some(message) = &self.inline_message {
            // Keep optional context after elapsed/interrupt text so that core
            // interrupt affordances stay in a fixed visual location.
            spans.push(" · ".dim());
            spans.push(message.clone().dim());
        }

        let mut lines = Vec::new();
        lines.push(truncate_line_with_ellipsis_if_overflow(
            Line::from(spans),
            usize::from(area.width),
        ));
        if area.height > 1 {
            // If there is enough space, add the details lines below the header.
            let details = self.wrapped_details_lines(area.width);
            let max_details = usize::from(area.height.saturating_sub(1));
            lines.extend(details.into_iter().take(max_details));
        }

        Paragraph::new(Text::from(lines)).render_ref(area, buf);
    }
}

fn compaction_part(header: &str) -> Option<&str> {
    let part = header.strip_prefix("Compacting part ")?;
    let (completed, total) = part.split_once('/')?;
    if completed.is_empty()
        || total.is_empty()
        || !completed
            .chars()
            .all(|character| character.is_ascii_digit())
        || !total.chars().all(|character| character.is_ascii_digit())
        || total.parse::<usize>().ok()? <= 1
    {
        return None;
    }
    Some(part)
}

fn compaction_stage(header: &str) -> Option<&str> {
    header
        .strip_prefix("Compacting ")
        .filter(|stage| !stage.is_empty())
}
