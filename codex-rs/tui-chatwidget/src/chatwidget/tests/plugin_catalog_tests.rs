use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn plugins_popup_uses_product_label_for_personal_tab() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![PluginMarketplaceEntry {
            name: "codex-curated".to_string(),
            path: Some(plugins_test_personal_marketplace_path()),
            interface: Some(MarketplaceInterface {
                display_name: Some("Personal".to_string()),
            }),
            plugins: vec![plugins_test_summary(
                "plugin-local-docs",
                "local-docs",
                Some("Local Docs"),
                Some("Local editable docs."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )],
        }]),
    );

    let popup = select_plugins_tab_containing(&mut chat, /*width*/ 120, "[Local]");
    assert!(
        popup.contains("Local.") && popup.contains("Local Docs") && !popup.contains("Personal."),
        "expected [Local] to use its product label, got:\n{popup}"
    );
    let row = popup
        .lines()
        .find(|line| line.contains("Local Docs"))
        .expect("expected plugin row")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    insta::assert_snapshot!(
        row,
        @"› [-] Local Docs Available Press Enter to install or view plugin details."
    );
}

#[tokio::test]
async fn plugin_detail_not_installable_plugin_disables_install_action() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = plugins_test_summary(
        "plugin-internal",
        "internal",
        Some("Internal"),
        Some("Internal only."),
        /*installed*/ false,
        /*enabled*/ true,
        PluginInstallPolicy::NotAvailable,
    );
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(vec![summary.clone()]),
        ])),
    );
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: plugins_test_detail(summary, Some("Internal only."), &[], &[], &[]),
        }),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    let install_row = popup
        .lines()
        .find(|line| line.contains("Install plugin"))
        .expect("expected install row");
    assert!(
        install_row.contains("This plugin is not installable from this marketplace."),
        "expected disabled not-installable row, got:\n{install_row}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    assert_eq!(
        render_bottom_popup(&chat, /*width*/ 100),
        popup,
        "expected navigation to skip the disabled install row"
    );
}
