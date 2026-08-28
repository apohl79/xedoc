//! Environment selection and shell state shared by a running Codex thread.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod environment;
pub mod environment_selection;
pub mod shell;
pub mod shell_snapshot;

pub use environment::TurnEnvironment;
pub use environment_selection::StartingTurnEnvironment;
pub use environment_selection::ThreadEnvironments;
pub use environment_selection::TurnEnvironmentSnapshot;
pub use environment_selection::TurnEnvironmentState;
pub use environment_selection::default_thread_environment_selections;
pub use shell_snapshot::ShellSnapshot;
pub use shell_snapshot::ShellSnapshotFile;
