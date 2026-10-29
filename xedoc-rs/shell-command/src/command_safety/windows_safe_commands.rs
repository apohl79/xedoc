use crate::command_safety::is_safe_command::is_safe_git_command;
use crate::command_safety::powershell_parser::PowershellParseOutcome;
use crate::command_safety::powershell_parser::parse_with_powershell_ast;
use std::path::Path;

/// On Windows, we conservatively allow only clearly read-only PowerShell invocations
/// that match a small safelist. Anything else (including direct CMD commands) is unsafe.
pub fn is_safe_command_windows(command: &[String]) -> bool {
    if let Some(commands) = try_parse_powershell_command_sequence(command) {
        commands
            .iter()
            .all(|cmd| is_safe_powershell_words(cmd.as_slice()))
    } else {
        // Only PowerShell invocations are allowed on Windows for now; anything else is unsafe.
        false
    }
}

/// Returns each command sequence if the invocation starts with a PowerShell binary.
/// For example, the tokens from `pwsh Get-ChildItem | Measure-Object` become two sequences.
fn try_parse_powershell_command_sequence(command: &[String]) -> Option<Vec<Vec<String>>> {
    let (exe, rest) = command.split_first()?;
    if is_powershell_executable(exe) {
        parse_powershell_invocation(exe, rest)
    } else {
        None
    }
}

/// Parses a PowerShell invocation into discrete command vectors, rejecting unsafe patterns.
fn parse_powershell_invocation(executable: &str, args: &[String]) -> Option<Vec<Vec<String>>> {
    if args.is_empty() {
        // Examples rejected here: "pwsh" and "powershell.exe" with no additional arguments.
        return None;
    }

    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "-command" | "/command" | "-c" => {
                let script = args.get(idx + 1)?;
                if idx + 2 != args.len() {
                    // Reject if there is more than one token representing the actual command.
                    // Examples rejected here: "pwsh -Command foo bar" and "powershell -c ls extra".
                    return None;
                }
                return parse_powershell_script(executable, script);
            }
            _ if lower.starts_with("-command:") || lower.starts_with("/command:") => {
                if idx + 1 != args.len() {
                    // Reject if there are more tokens after the command itself.
                    // Examples rejected here: "pwsh -Command:dir C:\\" and "powershell /Command:dir C:\\" with trailing args.
                    return None;
                }
                let script = arg.split_once(':')?.1;
                return parse_powershell_script(executable, script);
            }

            // Benign, no-arg flags we tolerate.
            "-nologo" | "-noprofile" | "-noninteractive" | "-mta" | "-sta" => {
                idx += 1;
                continue;
            }

            // Explicitly forbidden/opaque or unnecessary for read-only operations.
            "-encodedcommand" | "-ec" | "-file" | "/file" | "-windowstyle" | "-executionpolicy"
            | "-workingdirectory" => {
                // Examples rejected here: "pwsh -EncodedCommand ..." and "powershell -File script.ps1".
                return None;
            }

            // Unknown switch → bail conservatively.
            _ if lower.starts_with('-') => {
                // Examples rejected here: "pwsh -UnknownFlag" and "powershell -foo bar".
                return None;
            }

            // If we hit non-flag tokens, treat the remainder as a command sequence.
            // This happens if powershell is invoked without -Command, e.g.
            // ["pwsh", "-NoLogo", "git", "-c", "core.pager=cat", "status"]
            _ => {
                let script = join_arguments_as_script(&args[idx..]);
                return parse_powershell_script(executable, &script);
            }
        }
    }

    // Examples rejected here: "pwsh" and "powershell.exe -NoLogo" without a script.
    None
}

/// Tokenizes an inline PowerShell script and delegates to the command splitter.
/// Examples of when this is called: pwsh.exe -Command '<script>' or pwsh.exe -Command:<script>
fn parse_powershell_script(executable: &str, script: &str) -> Option<Vec<Vec<String>>> {
    if let PowershellParseOutcome::Commands(commands) =
        parse_with_powershell_ast(executable, script)
    {
        Some(commands)
    } else {
        None
    }
}

/// Returns true when the executable name is one of the supported PowerShell binaries.
fn is_powershell_executable(exe: &str) -> bool {
    let executable_name = Path::new(exe)
        .file_name()
        .and_then(|osstr| osstr.to_str())
        .unwrap_or(exe)
        .to_ascii_lowercase();

    matches!(
        executable_name.as_str(),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    )
}

fn join_arguments_as_script(args: &[String]) -> String {
    let mut words = Vec::with_capacity(args.len());
    if let Some((first, rest)) = args.split_first() {
        words.push(first.clone());
        for arg in rest {
            words.push(quote_argument(arg));
        }
    }
    words.join(" ")
}

fn quote_argument(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    if arg.chars().all(|ch| !ch.is_whitespace()) {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "''"))
}

/// Validates that a parsed PowerShell command stays within our read-only safelist.
/// Everything before this is parsing, and rejecting things that make us feel uncomfortable.
pub(crate) fn is_safe_powershell_words(words: &[String]) -> bool {
    if words.is_empty() {
        // Examples rejected here: "pwsh -Command ''" and "pwsh -Command \"\"".
        return false;
    }

    // Reject nested unsafe cmdlets inside parentheses or arguments
    for w in words.iter() {
        let inner = w
            .trim_matches(|c| c == '(' || c == ')')
            .trim_start_matches('-')
            .to_ascii_lowercase();
        if matches!(
            inner.as_str(),
            "set-content"
                | "add-content"
                | "out-file"
                | "new-item"
                | "remove-item"
                | "move-item"
                | "copy-item"
                | "rename-item"
                | "start-process"
                | "stop-process"
        ) {
            // Examples rejected here: "Write-Output (Set-Content foo6.txt 'abc')" and "Get-Content (New-Item bar.txt)".
            return false;
        }
    }

    let command = words[0]
        .trim_matches(|c| c == '(' || c == ')')
        .trim_start_matches('-')
        .to_ascii_lowercase();
    match command.as_str() {
        "echo" | "write-output" | "write-host" => true, // (no redirection allowed)
        "dir" | "ls" | "get-childitem" | "gci" => true,
        "cat" | "type" | "gc" | "get-content" => true,
        "select-string" | "sls" | "findstr" => true,
        "measure-object" | "measure" => true,
        "get-location" | "gl" | "pwd" => true,
        "test-path" | "tp" => true,
        "resolve-path" | "rvpa" => true,
        "select-object" | "select" => true,
        "get-item" => true,

        "git" => is_safe_git_command(words),

        "rg" => is_safe_ripgrep(words),

        // Extra safety: explicitly prohibit common side-effecting cmdlets regardless of args.
        "set-content" | "add-content" | "out-file" | "new-item" | "remove-item" | "move-item"
        | "copy-item" | "rename-item" | "start-process" | "stop-process" => {
            // Examples rejected here: "pwsh -Command 'Set-Content notes.txt data'" and "pwsh -Command 'Remove-Item temp.log'".
            false
        }

        _ => {
            // Examples rejected here: "pwsh -Command 'Invoke-WebRequest https://example.com'" and "pwsh -Command 'Start-Service Spooler'".
            false
        }
    }
}

/// Checks that an `rg` invocation avoids options that can spawn arbitrary executables.
fn is_safe_ripgrep(words: &[String]) -> bool {
    const UNSAFE_RIPGREP_OPTIONS_WITH_ARGS: &[&str] = &["--pre", "--hostname-bin"];
    const UNSAFE_RIPGREP_OPTIONS_WITHOUT_ARGS: &[&str] = &["--search-zip", "-z"];

    !words.iter().skip(1).any(|arg| {
        let arg_lc = arg.to_ascii_lowercase();
        // Examples rejected here: "pwsh -Command 'rg --pre cat pattern'" and "pwsh -Command 'rg --search-zip pattern'".
        UNSAFE_RIPGREP_OPTIONS_WITHOUT_ARGS.contains(&arg_lc.as_str())
            || UNSAFE_RIPGREP_OPTIONS_WITH_ARGS
                .iter()
                .any(|opt| arg_lc == *opt || arg_lc.starts_with(&format!("{opt}=")))
    })
}
