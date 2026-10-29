//! Terminal-specific row caps for resize reflow.
//!
//! The auto cap mirrors documented scrollback defaults for terminals we can identify. Console Host
//! does not expose its configured screen buffer through terminal metadata, so it usually lands in
//! the fallback bucket.
//!
//! These caps are deliberately conservative: Xedoc is rebuilding normal terminal scrollback, not an
//! internal virtual transcript. Replaying more rows than the terminal retains wastes work and can
//! make interactive resize feel worse without giving the user more usable history.

use xedoc_config::types::DEFAULT_TERMINAL_RESIZE_REFLOW_FALLBACK_MAX_ROWS;
use xedoc_terminal_detection::TerminalInfo;
use xedoc_terminal_detection::TerminalName;
use xedoc_terminal_detection::terminal_info;

use crate::legacy_core::config::TerminalResizeReflowConfig;
use crate::legacy_core::config::TerminalResizeReflowMaxRows;

const VSCODE_RESIZE_REFLOW_MAX_ROWS: usize = 1_000;
const WINDOWS_TERMINAL_RESIZE_REFLOW_MAX_ROWS: usize = 9_001;
const WEZTERM_RESIZE_REFLOW_MAX_ROWS: usize = 3_500;
const ALACRITTY_RESIZE_REFLOW_MAX_ROWS: usize = 10_000;

/// Resolve the configured row cap for resize and initial replay.
///
/// `Auto` uses terminal detection plus the VS Code environment probe because VS Code can run shells
/// whose terminal-name metadata points at the host shell rather than VS Code itself. Returning
/// `None` means the user explicitly disabled row limiting with `max_rows = 0`.
pub(crate) fn resize_reflow_max_rows(config: TerminalResizeReflowConfig) -> Option<usize> {
    resize_reflow_max_rows_for(
        config,
        &terminal_info(),
        crate::tui::running_in_vscode_terminal(),
    )
}

fn resize_reflow_max_rows_for(
    config: TerminalResizeReflowConfig,
    terminal: &TerminalInfo,
    running_in_vscode_terminal: bool,
) -> Option<usize> {
    match config.max_rows {
        TerminalResizeReflowMaxRows::Auto => Some(auto_resize_reflow_max_rows(
            terminal.name,
            running_in_vscode_terminal,
        )),
        TerminalResizeReflowMaxRows::Disabled => None,
        TerminalResizeReflowMaxRows::Limit(max_rows) => Some(max_rows),
    }
}

fn auto_resize_reflow_max_rows(
    terminal_name: TerminalName,
    running_in_vscode_terminal: bool,
) -> usize {
    if running_in_vscode_terminal {
        return VSCODE_RESIZE_REFLOW_MAX_ROWS;
    }

    match terminal_name {
        TerminalName::VsCode => VSCODE_RESIZE_REFLOW_MAX_ROWS,
        TerminalName::WindowsTerminal => WINDOWS_TERMINAL_RESIZE_REFLOW_MAX_ROWS,
        TerminalName::WezTerm => WEZTERM_RESIZE_REFLOW_MAX_ROWS,
        TerminalName::Alacritty => ALACRITTY_RESIZE_REFLOW_MAX_ROWS,
        TerminalName::AppleTerminal
        | TerminalName::Ghostty
        | TerminalName::Iterm2
        | TerminalName::WarpTerminal
        | TerminalName::Kitty
        | TerminalName::Konsole
        | TerminalName::GnomeTerminal
        | TerminalName::Vte
        | TerminalName::Dumb
        | TerminalName::Unknown => DEFAULT_TERMINAL_RESIZE_REFLOW_FALLBACK_MAX_ROWS,
    }
}
