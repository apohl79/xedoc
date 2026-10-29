use std::fmt;
use std::io;
use std::io::stdout;

use crossterm::Command;
use ratatui::crossterm::execute;
use xedoc_terminal_detection::Multiplexer;
use xedoc_terminal_detection::terminal_info;

#[derive(Debug)]
pub struct Osc9Backend {
    dcs_passthrough: bool,
}

impl Default for Osc9Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Osc9Backend {
    pub fn new() -> Self {
        Self {
            dcs_passthrough: matches!(terminal_info().multiplexer, Some(Multiplexer::Tmux { .. })),
        }
    }

    pub fn notify(&mut self, message: &str) -> io::Result<()> {
        execute!(
            stdout(),
            PostNotification {
                message: message.to_string(),
                dcs_passthrough: self.dcs_passthrough,
            }
        )
    }
}

/// Command that emits an OSC 9 desktop notification with a message.
#[derive(Debug, Clone)]
pub struct PostNotification {
    pub message: String,
    pub dcs_passthrough: bool,
}

impl Command for PostNotification {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        if self.dcs_passthrough {
            let escaped_message = escape_tmux_dcs_passthrough_payload(&self.message);
            write!(f, "\x1bPtmux;\x1b\x1b]9;{escaped_message}\x07\x1b\\")
        } else {
            write!(f, "\x1b]9;{}\x07", self.message)
        }
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(std::io::Error::other(
            "tried to execute PostNotification using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

fn escape_tmux_dcs_passthrough_payload(message: &str) -> String {
    message.replace('\u{1b}', "\u{1b}\u{1b}")
}
