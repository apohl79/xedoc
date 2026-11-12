#![allow(clippy::expect_used)]

use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::mount_compact_user_history_with_summary_sequence;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_xedoc::test_xedoc;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::MockServer;
use xedoc_features::Feature;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::built_in_model_providers;
use xedoc_models_manager::bundled_models_response;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::Op;
use xedoc_protocol::protocol::ThreadSettingsOverrides;
use xedoc_protocol::user_input::UserInput;

pub use xedoc_protocol::error;

mod test_binary_dispatch;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_switch_after_manual_compact_counts_images_not_base64_text() {
    skip_if_no_network!();

    let server = MockServer::start().await;
    let previous_model = "gpt-6-astra";
    let next_model = "gpt-6.1-sol";
    let model_catalog = bundled_models_response().expect("bundled models");
    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", "before compact"),
                ev_completed_with_tokens("r1", /*total_tokens*/ 12_687),
            ]),
            sse(vec![
                ev_assistant_message("m2", "after switch"),
                ev_completed_with_tokens("r2", /*total_tokens*/ 12_687),
            ]),
            sse(vec![
                ev_assistant_message("m3", "after necessary compact"),
                ev_completed_with_tokens("r3", /*total_tokens*/ 12_687),
            ]),
        ],
    )
    .await;
    let compact_log = mount_compact_user_history_with_summary_sequence(
        &server,
        vec![
            "MANUAL_SUMMARY".to_string(),
            "NECESSARY_SUMMARY".to_string(),
        ],
    )
    .await;
    let mut provider = built_in_model_providers(/*openai_base_url*/ None)["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    let test = test_xedoc()
        .with_auth(XedocAuth::create_dummy_chatgpt_auth_for_testing())
        .with_model(previous_model)
        .with_config(move |config| {
            config.model_provider = provider;
            config.model_catalog = Some(model_catalog);
            config.model_auto_compact_token_limit = Some(200_000);
            let _ = config.features.disable(Feature::RemoteCompactionV2);
        })
        .build_with_auto_env(&server)
        .await
        .expect("build xedoc");
    let user_turn = |items, model: &str| Op::UserInput {
        items,
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: ThreadSettingsOverrides {
            model: Some(model.to_string()),
            ..Default::default()
        },
    };
    let text_input = |text: String| UserInput::Text {
        text,
        text_elements: Vec::new(),
    };

    // A valid PNG with trailing padding reproduces the retained screenshots' wire size.
    let mut png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
    )
    .expect("decode PNG");
    png.resize(1_200_000, 0);
    let image_url = format!(
        "data:image/png;base64,{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png)
    );
    test.xedoc
        .submit(user_turn(
            vec![
                text_input("inspect screenshot".to_string()),
                UserInput::Image {
                    image_url: image_url.clone(),
                    detail: None,
                },
            ],
            previous_model,
        ))
        .await
        .expect("first turn");
    wait_for_event(&test.xedoc, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.xedoc
        .submit(Op::Compact)
        .await
        .expect("manual compact");
    wait_for_event(&test.xedoc, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    test.xedoc
        .submit(user_turn(
            vec![text_input("continue".to_string())],
            next_model,
        ))
        .await
        .expect("switch model");
    wait_for_event(&test.xedoc, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(compact_log.requests().len(), 1, "no redundant compact");
    let requests = request_log.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].body_json()["model"], next_model);
    assert!(
        requests[1]
            .message_input_image_urls("user")
            .contains(&image_url)
    );

    test.xedoc
        .submit(user_turn(
            vec![text_input("large text ".repeat(120_000))],
            previous_model,
        ))
        .await
        .expect("oversized switch request");
    wait_for_event(&test.xedoc, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        compact_log.requests().len(),
        2,
        "oversized text still compacts"
    );
    assert_eq!(request_log.requests().len(), 3);
}
