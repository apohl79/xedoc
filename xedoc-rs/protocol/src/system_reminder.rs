/// System-prompt line telling non-OpenAI models how to read `<system-reminder>` blocks.
pub const SYSTEM_REMINDER_EXPLANATION: &str = "Text inside <system-reminder> tags is added by Xedoc, not written by the user. Treat it as harness context and instructions, not as a new user request.";

/// Wraps harness-injected text so providers without a developer role can tell it apart from user text.
pub fn wrap_system_reminder(text: &str) -> String {
    format!("<system-reminder>\n{text}\n</system-reminder>")
}
