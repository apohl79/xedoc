//! Shared helpers for filtering and matching slash commands.
//!
//! The same sandbox- and feature-gating rules are used by both the composer
//! and the command popup. Centralizing them here keeps those call sites small
//! and ensures they stay in sync.
use std::str::FromStr;

use xedoc_utils_fuzzy_match::fuzzy_match;

use crate::slash_command::SlashCommand;
use crate::slash_command::built_in_slash_commands;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceTierCommand {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// An approved session extension command exposed through the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionExtensionCommand {
    /// Stable identity of the extension that declared this command.
    pub extension_id: String,
    /// Slash command name without a leading slash.
    pub name: String,
    /// Human-readable command description.
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommandItem {
    Builtin(SlashCommand),
    ServiceTier(ServiceTierCommand),
    SessionExtension(SessionExtensionCommand),
}

impl SlashCommandItem {
    pub fn command(&self) -> &str {
        match self {
            Self::Builtin(cmd) => cmd.command(),
            Self::ServiceTier(command) => &command.name,
            Self::SessionExtension(command) => &command.name,
        }
    }

    pub fn supports_inline_args(&self) -> bool {
        match self {
            Self::Builtin(cmd) => cmd.supports_inline_args(),
            Self::ServiceTier(_) => false,
            Self::SessionExtension(_) => true,
        }
    }

    pub fn available_in_side_conversation(&self) -> bool {
        match self {
            Self::Builtin(cmd) => cmd.available_in_side_conversation(),
            Self::ServiceTier(_) => false,
            Self::SessionExtension(_) => false,
        }
    }

    pub fn available_during_task(&self) -> bool {
        match self {
            Self::Builtin(cmd) => cmd.available_during_task(),
            Self::ServiceTier(_) => true,
            Self::SessionExtension(_) => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinCommandFlags {
    pub collaboration_modes_enabled: bool,
    pub plugins_command_enabled: bool,
    pub service_tier_commands_enabled: bool,
    pub goal_command_enabled: bool,
    pub personality_command_enabled: bool,
    pub side_conversation_active: bool,
}

/// Return the built-ins that should be visible/usable for the current input.
pub fn builtins_for_input(flags: BuiltinCommandFlags) -> Vec<(&'static str, SlashCommand)> {
    built_in_slash_commands()
        .into_iter()
        .filter(|(_, cmd)| flags.collaboration_modes_enabled || *cmd != SlashCommand::Plan)
        .filter(|(_, cmd)| flags.plugins_command_enabled || *cmd != SlashCommand::Plugins)
        .filter(|(_, cmd)| flags.goal_command_enabled || *cmd != SlashCommand::Goal)
        .filter(|(_, cmd)| flags.personality_command_enabled || *cmd != SlashCommand::Personality)
        .filter(|(_, cmd)| !flags.side_conversation_active || cmd.available_in_side_conversation())
        .collect()
}

pub fn commands_for_input(
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> Vec<SlashCommandItem> {
    commands_for_input_with_session_extensions(flags, service_tier_commands, &[])
}

/// Return commands available to the composer, including approved session extensions.
pub fn commands_for_input_with_session_extensions(
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
    session_extension_commands: &[SessionExtensionCommand],
) -> Vec<SlashCommandItem> {
    let mut commands = Vec::new();
    let tiers_enabled = flags.service_tier_commands_enabled;
    for (_, cmd) in builtins_for_input(flags) {
        commands.push(SlashCommandItem::Builtin(cmd));
        if cmd == SlashCommand::Model && tiers_enabled {
            commands.extend(
                service_tier_commands
                    .iter()
                    .cloned()
                    .map(SlashCommandItem::ServiceTier),
            );
        }
    }
    commands.extend(
        session_extension_commands
            .iter()
            .filter(|command| find_builtin_command(&command.name, flags).is_none())
            .cloned()
            .map(SlashCommandItem::SessionExtension),
    );
    commands
        .into_iter()
        .filter(|cmd| !flags.side_conversation_active || cmd.available_in_side_conversation())
        .collect()
}

/// Find a single built-in command by a recognized name or alias, after applying feature gating.
///
/// Side-conversation and token-activity gating are intentionally enforced by dispatch rather than
/// command lookup so a typed command can produce a specific unavailable message while the popup
/// still hides it.
pub fn find_builtin_command(name: &str, flags: BuiltinCommandFlags) -> Option<SlashCommand> {
    let cmd = SlashCommand::from_str(name).ok().or_else(|| {
        let repeated_os = name.strip_prefix('g')?.strip_suffix("al")?;
        (!repeated_os.is_empty() && repeated_os.bytes().all(|byte| byte == b'o'))
            .then_some(SlashCommand::Goal)
    })?;
    builtins_for_input(BuiltinCommandFlags {
        side_conversation_active: false,
        ..flags
    })
    .into_iter()
    .any(|(_, visible_cmd)| visible_cmd == cmd)
    .then_some(cmd)
}

pub fn find_slash_command(
    name: &str,
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> Option<SlashCommandItem> {
    find_slash_command_with_session_extensions(name, flags, service_tier_commands, &[])
}

/// Find a command by name, preferring built-ins over dynamic commands.
pub fn find_slash_command_with_session_extensions(
    name: &str,
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
    session_extension_commands: &[SessionExtensionCommand],
) -> Option<SlashCommandItem> {
    if let Some(cmd) = find_builtin_command(name, flags) {
        return Some(SlashCommandItem::Builtin(cmd));
    }

    let tiers_enabled = flags.service_tier_commands_enabled;
    if let Some(Some(command)) = tiers_enabled.then(|| {
        service_tier_commands
            .iter()
            .find(|command| command.name.as_str() == name)
            .cloned()
            .map(SlashCommandItem::ServiceTier)
    }) {
        return Some(command);
    }

    session_extension_commands
        .iter()
        .find(|command| command.name.as_str() == name)
        .cloned()
        .map(SlashCommandItem::SessionExtension)
}

pub fn has_slash_command_prefix(
    name: &str,
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
) -> bool {
    has_slash_command_prefix_with_session_extensions(name, flags, service_tier_commands, &[])
}

/// Return whether input is a prefix of a command available to the composer.
pub fn has_slash_command_prefix_with_session_extensions(
    name: &str,
    flags: BuiltinCommandFlags,
    service_tier_commands: &[ServiceTierCommand],
    session_extension_commands: &[SessionExtensionCommand],
) -> bool {
    commands_for_input_with_session_extensions(
        flags,
        service_tier_commands,
        session_extension_commands,
    )
    .into_iter()
    .any(|command| fuzzy_match(command.command(), name).is_some())
}
