//! Narrow conversion helpers for approval-related app-server payloads.
//!
//! The TUI mostly keeps app-server approval types intact. These helpers cover
//! the remaining cases where the UI consumes a private file-change display
//! model or needs to translate a granted permission response for outbound
//! submission.

use std::collections::HashMap;
use std::path::PathBuf;
use xedoc_app_server_protocol::AdditionalNetworkPermissions;
use xedoc_app_server_protocol::FileUpdateChange;
use xedoc_app_server_protocol::GrantedPermissionProfile;
use xedoc_app_server_protocol::PatchChangeKind;
use xedoc_protocol::request_permissions::RequestPermissionProfile as CoreRequestPermissionProfile;
use xedoc_tui_render::diff_model::FileChange;

pub fn granted_permission_profile_from_request(
    value: CoreRequestPermissionProfile,
) -> GrantedPermissionProfile {
    GrantedPermissionProfile {
        network: value.network.map(|network| AdditionalNetworkPermissions {
            enabled: network.enabled,
        }),
        file_system: value.file_system.map(Into::into),
    }
}

pub fn file_update_changes_to_display(
    changes: Vec<FileUpdateChange>,
) -> HashMap<PathBuf, FileChange> {
    changes
        .into_iter()
        .map(|change| {
            let path = PathBuf::from(change.path);
            let file_change = match change.kind {
                PatchChangeKind::Add => FileChange::Add {
                    content: change.diff,
                },
                PatchChangeKind::Delete => FileChange::Delete {
                    content: change.diff,
                },
                PatchChangeKind::Update { move_path } => FileChange::Update {
                    unified_diff: change.diff,
                    move_path,
                },
            };
            (path, file_change)
        })
        .collect()
}
