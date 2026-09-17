//! Approved session-extension command state for the composer.

use super::ChatWidget;
use crate::bottom_pane::slash_commands::SessionExtensionCommand;

impl ChatWidget {
    pub fn set_session_extension_commands(&mut self, commands: Vec<SessionExtensionCommand>) {
        self.session_extension_commands = commands.clone();
        self.bottom_pane.set_session_extension_commands(commands);
    }
}
