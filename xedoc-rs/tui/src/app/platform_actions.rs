//! Platform-specific app actions and small global shortcuts.
//!
//! This module owns the side-conversation return shortcut predicate.

use super::*;

pub(super) fn side_return_shortcut_matches(key_event: KeyEvent) -> bool {
    matches!(
        key_event,
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL)
            && (c.eq_ignore_ascii_case(&'c') || c.eq_ignore_ascii_case(&'d'))
    )
}
