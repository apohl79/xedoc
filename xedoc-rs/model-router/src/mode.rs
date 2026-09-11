use crate::RouteScope;

/// Configured scope and application behavior for automatic model routing.
///
/// Classification and route application are intentionally separate: callers
/// first obtain a shared decision, then use this mode to decide whether that
/// proposal may affect the task's effective route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterMode {
    Off,
    ShadowSubagents,
    ShadowFull,
    Subagents,
    Full,
}

impl RouterMode {
    /// Whether eligible tasks in this scope should be classified.
    pub const fn classifies(self, scope: RouteScope) -> bool {
        match (self, scope) {
            (Self::Off, _) => false,
            (Self::ShadowSubagents | Self::Subagents, RouteScope::Root) => false,
            (Self::ShadowSubagents | Self::Subagents, RouteScope::Subagent)
            | (Self::ShadowFull | Self::Full, _) => true,
        }
    }

    /// Whether a classified proposal may replace the pre-router route.
    pub const fn applies(self, scope: RouteScope) -> bool {
        matches!(
            (self, scope),
            (Self::Subagents, RouteScope::Subagent) | (Self::Full, _)
        )
    }
}
