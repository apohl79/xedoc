/// Update action the CLI should perform after the TUI exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Update via the SHA-256-verified Xedoc macOS package installer, then relaunch.
    StandaloneMacos,
}

impl UpdateAction {
    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            UpdateAction::StandaloneMacos => (
                "sh",
                &[
                    "-c",
                    "curl -fsSL https://raw.githubusercontent.com/apohl79/codex/main-fork/scripts/install/install.sh | sh",
                ],
            ),
        }
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(self) -> String {
        let (command, args) = self.command_args();
        shlex::try_join(std::iter::once(command).chain(args.iter().copied()))
            .unwrap_or_else(|_| format!("{command} {}", args.join(" ")))
    }
}

/// Returns the update action for this build, if Xedoc can update itself on this platform.
#[cfg(not(debug_assertions))]
pub fn get_update_action() -> Option<UpdateAction> {
    cfg!(target_os = "macos").then_some(UpdateAction::StandaloneMacos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn standalone_update_command_runs_installer() {
        assert_eq!(
            UpdateAction::StandaloneMacos.command_args(),
            (
                "sh",
                &[
                    "-c",
                    "curl -fsSL https://raw.githubusercontent.com/apohl79/codex/main-fork/scripts/install/install.sh | sh"
                ][..],
            )
        );
    }
}
