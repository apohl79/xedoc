use super::*;
use crate::shell::default_user_shell;
use codex_extension_api::PromptSlot;
use codex_plugin::manifest::PluginContextPosition as ManifestPosition;

fn contributor(condition_shell: Option<&str>) -> PluginManifestContextContributor {
    let cwd = AbsolutePathBuf::from_absolute_path_checked(
        std::env::current_dir().expect("test current directory"),
    )
    .expect("absolute test current directory");
    PluginManifestContextContributor {
        cache: PluginContextCache::new(vec![(
            "test-plugin".to_string(),
            PluginManifestContext {
                thread: vec![PluginThreadContextEntry {
                    slot: PluginContextSlot::ContextualUser,
                    position: ManifestPosition::Supplement,
                    text: "conditional context".to_string(),
                    condition_shell: condition_shell.map(str::to_string),
                }],
            },
        )]),
        shell: Arc::new(default_user_shell()),
        cwd,
    }
}

async fn fragments_for(condition_shell: Option<&str>) -> Vec<PromptFragment> {
    let contributor = contributor(condition_shell);
    let session_store = ExtensionData::new("session".to_string());
    let thread_store = ExtensionData::new("thread".to_string());
    contributor
        .contribute_thread_context(&session_store, &thread_store)
        .await
}

#[tokio::test]
async fn plugin_context_without_condition_shell_is_injected() {
    assert_eq!(
        fragments_for(None).await,
        vec![
            PromptFragment::new(PromptSlot::ContextualUser, "conditional context")
                .with_position(PluginContextPosition::Supplement)
        ]
    );
}

#[tokio::test]
async fn plugin_context_condition_shell_exit_zero_is_injected() {
    assert_eq!(
        fragments_for(Some("exit 0")).await,
        vec![
            PromptFragment::new(PromptSlot::ContextualUser, "conditional context")
                .with_position(PluginContextPosition::Supplement)
        ]
    );
}

#[tokio::test]
async fn plugin_context_condition_shell_nonzero_exit_is_dropped() {
    assert_eq!(fragments_for(Some("exit 1")).await, Vec::new());
}
