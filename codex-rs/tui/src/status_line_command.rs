//! External status-line command execution.
//!
//! The TUI renders the built-in status line synchronously, but custom commands
//! run off the draw path and return a single ANSI-capable line when complete.

use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use codex_ansi_escape::ansi_escape_line;
use codex_config::types::StatusLineCommand;
use codex_shell_command::shell_detect::ShellType;
use codex_shell_command::shell_detect::default_user_shell;
use ratatui::text::Line;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

const STATUS_LINE_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const STATUS_LINE_COMMAND_STDERR_LOG_CAP: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
}

pub(crate) async fn run_status_line_command(
    config: StatusLineCommand,
    payload: String,
    cwd: PathBuf,
) -> Option<Line<'static>> {
    let Some(spec) = command_spec_for_config(&config) else {
        tracing::debug!("status line command is empty");
        return None;
    };

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!(error = %err, program = %spec.program, "failed to spawn status line command");
            return None;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            if let Err(err) = stdin.write_all(payload.as_bytes()).await {
                tracing::debug!(error = %err, "failed to write status line command payload");
            }
        });
    }

    let output = match timeout(STATUS_LINE_COMMAND_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, program = %spec.program, "status line command failed");
            return None;
        }
        Err(_) => {
            tracing::debug!(program = %spec.program, "status line command timed out");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::debug!(
            status = %output.status,
            stderr = %truncate_for_log(&stderr),
            "status line command exited unsuccessfully"
        );
        return None;
    }

    line_from_stdout(&String::from_utf8_lossy(&output.stdout))
}

fn command_spec_for_config(config: &StatusLineCommand) -> Option<CommandSpec> {
    match config {
        StatusLineCommand::Command(command) => command_spec_for_shell_command(command),
        StatusLineCommand::Args(args) => command_spec_for_args(args),
    }
}

fn command_spec_for_shell_command(command: &str) -> Option<CommandSpec> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }

    let shell = default_user_shell();
    let program = shell.shell_path.to_string_lossy().to_string();
    let args = match shell.shell_type {
        ShellType::PowerShell => vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ],
        ShellType::Cmd => vec!["/C".to_string(), command.to_string()],
        ShellType::Zsh | ShellType::Bash | ShellType::Sh => {
            vec!["-lc".to_string(), command.to_string()]
        }
    };
    Some(CommandSpec { program, args })
}

fn command_spec_for_args(args: &[String]) -> Option<CommandSpec> {
    let (program, rest) = args.split_first()?;
    if program.trim().is_empty() {
        return None;
    }
    Some(CommandSpec {
        program: expand_program_home(program),
        args: rest.to_vec(),
    })
}

fn expand_program_home(program: &str) -> String {
    let Some(stripped) = program.strip_prefix("~/") else {
        return program.to_string();
    };
    let Some(home) = dirs::home_dir() else {
        return program.to_string();
    };
    home.join(stripped).to_string_lossy().to_string()
}

fn line_from_stdout(stdout: &str) -> Option<Line<'static>> {
    let first_line = stdout.lines().next().unwrap_or(stdout).trim_end();
    if first_line.is_empty() {
        None
    } else {
        Some(ansi_escape_line(first_line))
    }
}

fn truncate_for_log(value: &str) -> String {
    let value = value.trim();
    if value.len() <= STATUS_LINE_COMMAND_STDERR_LOG_CAP {
        return value.to_string();
    }

    let mut truncated = value
        .chars()
        .take(STATUS_LINE_COMMAND_STDERR_LOG_CAP)
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

#[allow(dead_code)]
pub(crate) fn configured_command_program(config: &StatusLineCommand) -> Option<String> {
    command_spec_for_config(config).map(|spec| {
        Path::new(&spec.program)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or(spec.program)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn args_command_expands_home_for_program() {
        let spec =
            command_spec_for_args(&["~/.claude/statusline.sh".to_string(), "--flag".to_string()])
                .expect("command spec");

        assert!(
            spec.program.ends_with(".claude/statusline.sh")
                || spec.program.ends_with(".claude\\statusline.sh")
        );
        assert_eq!(spec.args, vec!["--flag".to_string()]);
    }

    #[test]
    fn stdout_to_line_uses_first_line_and_parses_ansi() {
        let line = line_from_stdout("\u{1b}[32mok\u{1b}[0m\nignored").expect("line");

        assert_eq!(line_text(&line), "ok");
    }
}
