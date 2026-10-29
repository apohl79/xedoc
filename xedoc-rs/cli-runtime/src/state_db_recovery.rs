//! CLI handling for local state database startup failures.
//!
//! This keeps user-facing backup and lock-contention handling out of the main
//! CLI dispatch path while preserving the TUI startup error as the boundary type.

use std::io::IsTerminal;
use std::path::Path;
use xedoc_state::RuntimeDbBackup;
use xedoc_tui::LocalStateDbStartupError;

pub(crate) fn startup_error(err: &std::io::Error) -> Option<&LocalStateDbStartupError> {
    err.get_ref()
        .and_then(|err| err.downcast_ref::<LocalStateDbStartupError>())
}

pub(crate) fn is_locked(detail: &str) -> bool {
    xedoc_state::sqlite_error_detail_is_lock(detail)
}

pub(crate) fn is_corruption(detail: &str) -> bool {
    xedoc_state::sqlite_error_detail_is_corruption(detail)
}

pub(crate) fn is_auto_backup_recoverable(startup_error: &LocalStateDbStartupError) -> bool {
    is_corruption(startup_error.detail()) || sqlite_home_is_blocking_file(startup_error)
}

fn sqlite_home_is_blocking_file(startup_error: &LocalStateDbStartupError) -> bool {
    startup_error
        .database_path()
        .parent()
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.is_file())
}

pub(crate) fn print_auto_backup_start(startup_error: &LocalStateDbStartupError) {
    eprintln!("Xedoc couldn't start because its local database appears to be damaged.");
    eprintln!("Moving the damaged local database aside so Xedoc can rebuild it from saved data.");
    print_technical_details(startup_error);
}

pub(crate) async fn backup_files_for_fresh_start(
    startup_error: &LocalStateDbStartupError,
) -> std::io::Result<Vec<RuntimeDbBackup>> {
    xedoc_state::backup_runtime_db_for_fresh_start(startup_error.database_path()).await
}

pub(crate) fn confirm_fresh_start_rebuild(
    startup_error: &LocalStateDbStartupError,
    backups: &[RuntimeDbBackup],
) -> std::io::Result<()> {
    eprintln!("Xedoc rebuilt its local database.");
    eprintln!(
        "Xedoc detected a damaged local database, moved it into a backup folder, and will continue startup with a fresh database."
    );
    eprintln!("Database path: {}", startup_error.database_path().display());
    if let Some(backup_folder) = backup_folder(backups) {
        eprintln!("Backup folder: {}", backup_folder.display());
    } else {
        eprintln!("Backup folder: unavailable");
    }

    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        eprintln!("Press Enter to continue.");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
    } else {
        eprintln!("Continuing startup with a fresh local database...");
    }
    Ok(())
}

pub(crate) fn print_diagnostic_guidance(startup_error: &LocalStateDbStartupError) {
    eprintln!("Xedoc couldn't start because its local database appears to be damaged.");
    eprintln!("If this keeps happening, share the technical details below when asking for help.");
    print_technical_details(startup_error);
}

pub(crate) fn print_locked_guidance(startup_error: &LocalStateDbStartupError) {
    eprintln!("Xedoc couldn't start because another Xedoc process is using its local data.");
    eprintln!("Quit any other copies of Xedoc that may still be running, then try again.");
    print_technical_details(startup_error);
}

fn print_technical_details(startup_error: &LocalStateDbStartupError) {
    eprintln!("Technical details:");
    eprintln!("  Location: {}", startup_error.database_path().display());
    eprintln!("  Cause: {}", startup_error.detail());
}

fn backup_folder(backups: &[RuntimeDbBackup]) -> Option<&Path> {
    backups.first()?.backup_path.parent()
}
