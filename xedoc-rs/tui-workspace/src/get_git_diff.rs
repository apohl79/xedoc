//! Utility to compute the current Git diff for the working directory.
//!
//! The implementation mirrors the behaviour of the TypeScript version in
//! `xedoc-cli`: it returns the diff for tracked changes as well as any
//! untracked files. When the current directory is not inside a Git
//! repository, the function returns `Ok((false, String::new()))`.

use std::path::Path;
use std::time::Duration;

use crate::workspace_command::WorkspaceCommand;
use crate::workspace_command::WorkspaceCommandExecutor;
use crate::workspace_command::WorkspaceCommandOutput;
use xedoc_git_utils::FsmonitorOverride;
use xedoc_git_utils::FsmonitorProbeRunner;
use xedoc_git_utils::detect_fsmonitor_override;

const DIFF_COMMAND_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 30);
const DISABLE_HOOKS_CONFIG: &str = if cfg!(windows) {
    "core.hooksPath=NUL"
} else {
    "core.hooksPath=/dev/null"
};
const EXECUTABLE_FILTER_CONFIG_PATTERN: &str = r"^filter\..*\.(clean|process)$";

// `/diff` may execute Git through a remote workspace, so git-utils owns the
// probe policy while this adapter keeps command execution in the TUI layer.
// WorkspaceCommand bounds each call; `/diff` has no aggregate command deadline.
struct WorkspaceFsmonitorProbeRunner<'a> {
    runner: &'a dyn WorkspaceCommandExecutor,
    cwd: &'a Path,
}

impl FsmonitorProbeRunner for WorkspaceFsmonitorProbeRunner<'_> {
    async fn run_probe(&mut self, args: &[&str]) -> Option<Vec<u8>> {
        let argv = ["git"].into_iter().chain(args.iter().copied());
        let command = WorkspaceCommand::new(argv).cwd(self.cwd.to_path_buf());
        match self.runner.run(command).await {
            Ok(output) if output.success() => Some(output.stdout.into_bytes()),
            _ => None,
        }
    }
}

/// Return value of [`get_git_diff`].
///
/// * `bool` – Whether the current working directory is inside a Git repo.
/// * `String` – The concatenated diff (may be empty).
pub async fn get_git_diff(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
) -> Result<(bool, String), String> {
    // First check if we are inside a Git repository.
    if !inside_git_repo(runner, cwd).await? {
        return Ok((false, String::new()));
    }

    // Probe once per `/diff` and reuse the result for all subsequent Git commands.
    let mut probe_runner = WorkspaceFsmonitorProbeRunner { runner, cwd };
    let fsmonitor = detect_fsmonitor_override(&mut probe_runner).await;

    // Keep `/diff` informational: repository configuration must not select executable diff helpers.
    let diff_config_overrides = diff_filter_config_overrides(runner, cwd, fsmonitor).await?;
    let (tracked_diff_res, untracked_output_res) = tokio::join!(
        run_git_capture_diff(
            runner,
            cwd,
            fsmonitor,
            &diff_config_overrides,
            &[
                "diff",
                "--no-textconv",
                "--no-ext-diff",
                "--submodule=short",
                "--ignore-submodules=dirty",
                "--color",
            ]
        ),
        run_git_capture_stdout(
            runner,
            cwd,
            fsmonitor,
            &["ls-files", "--others", "--exclude-standard"]
        ),
    );
    let tracked_diff = tracked_diff_res?;
    let untracked_output = untracked_output_res?;

    let mut untracked_diff = String::new();
    let null_device: &Path = if cfg!(windows) {
        Path::new("NUL")
    } else {
        Path::new("/dev/null")
    };

    let null_path = null_device.to_str().unwrap_or("/dev/null");
    for file in untracked_output
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let args = [
            "diff",
            "--no-textconv",
            "--no-ext-diff",
            "--submodule=short",
            "--ignore-submodules=dirty",
            "--color",
            "--no-index",
            "--",
            null_path,
            file,
        ];
        let diff =
            run_git_capture_diff(runner, cwd, fsmonitor, &diff_config_overrides, &args).await?;
        untracked_diff.push_str(&diff);
    }

    Ok((true, format!("{tracked_diff}{untracked_diff}")))
}

/// Helper that executes `git` with the given `args` and returns `stdout` as a
/// UTF-8 string. Any non-zero exit status is considered an *error*.
async fn run_git_capture_stdout(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
    fsmonitor: FsmonitorOverride,
    args: &[&str],
) -> Result<String, String> {
    let output = run_git_command(runner, cwd, fsmonitor, &[], args).await?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {:?} failed with status {}",
            args, output.exit_code
        ))
    }
}

/// Like [`run_git_capture_stdout`] but treats exit status 1 as success and
/// returns stdout. Git returns 1 for diffs when differences are present.
async fn run_git_capture_diff(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
    fsmonitor: FsmonitorOverride,
    config_overrides: &[(String, String)],
    args: &[&str],
) -> Result<String, String> {
    let output = run_git_command(runner, cwd, fsmonitor, config_overrides, args).await?;
    if output.success() || output.exit_code == 1 {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {:?} failed with status {}",
            args, output.exit_code
        ))
    }
}

/// Return Git configuration overrides that prevent configured filter drivers
/// from executing while generating diffs.
async fn diff_filter_config_overrides(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
    fsmonitor: FsmonitorOverride,
) -> Result<Vec<(String, String)>, String> {
    let args = [
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        EXECUTABLE_FILTER_CONFIG_PATTERN,
    ];
    let output = run_git_command(runner, cwd, fsmonitor, &[], &args).await?;
    if output.exit_code != 0 && output.exit_code != 1 {
        return Err(format!(
            "git {:?} failed with status {}",
            args, output.exit_code
        ));
    }

    let mut drivers = output
        .stdout
        .split('\0')
        .filter_map(|key| {
            key.strip_suffix(".clean")
                .or_else(|| key.strip_suffix(".process"))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    drivers.sort();
    drivers.dedup();

    Ok(drivers
        .into_iter()
        .flat_map(|driver| {
            [
                (format!("{driver}.clean"), String::new()),
                (format!("{driver}.process"), String::new()),
                (format!("{driver}.required"), "false".to_string()),
            ]
        })
        .collect())
}

/// Determine if the current directory is inside a Git repository.
async fn inside_git_repo(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
) -> Result<bool, String> {
    // `rev-parse` does not inspect the worktree, and probing before this check
    // would also run extra Git commands outside repositories.
    let output = run_git_command(
        runner,
        cwd,
        FsmonitorOverride::Disabled,
        &[],
        &["rev-parse", "--is-inside-work-tree"],
    )
    .await?;
    Ok(output.success())
}

async fn run_git_command(
    runner: &dyn WorkspaceCommandExecutor,
    cwd: &Path,
    fsmonitor: FsmonitorOverride,
    config_overrides: &[(String, String)],
    args: &[&str],
) -> Result<WorkspaceCommandOutput, String> {
    let argv = [
        "git",
        "-c",
        fsmonitor.git_config_arg(),
        "-c",
        DISABLE_HOOKS_CONFIG,
    ]
    .into_iter()
    .chain(args.iter().copied());
    let mut command = WorkspaceCommand::new(argv)
        .cwd(cwd.to_path_buf())
        .timeout(DIFF_COMMAND_TIMEOUT)
        .disable_output_cap();
    if !config_overrides.is_empty() {
        command = command.env("GIT_CONFIG_COUNT", config_overrides.len().to_string());
        for (index, (key, value)) in config_overrides.iter().enumerate() {
            command = command
                .env(format!("GIT_CONFIG_KEY_{index}"), key)
                .env(format!("GIT_CONFIG_VALUE_{index}"), value);
        }
    }
    runner.run(command).await.map_err(|err| err.to_string())
}
